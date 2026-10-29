//! The macOS permissions Mu needs, checked live so they can all be granted upfront instead of
//! failing one at a time later. The terminal's commands count as Mu too, so these cover them.

mod guide;

use std::{
    cell::Cell, collections::HashMap, ffi::c_void, mem, net::UdpSocket, ptr, rc::Rc, sync::LazyLock, time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use objc2_core_foundation::CFString;

use guide::Guide;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    FullDiskAccess,
    AppManagement,
    LocalNetwork,
    DeveloperTools,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Granted,
    Missing,
    /// Not checked yet, or Local Network couldn't be tried.
    Unknown,
}

impl Permission {
    pub const ALL: [Permission; 4] =
        [Permission::FullDiskAccess, Permission::AppManagement, Permission::LocalNetwork, Permission::DeveloperTools];

    fn name(self) -> &'static str {
        match self {
            Permission::FullDiskAccess => "Full Disk Access",
            Permission::AppManagement => "App Management",
            Permission::LocalNetwork => "Local Network",
            Permission::DeveloperTools => "Developer Tools",
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Permission::FullDiskAccess => "Open every folder, including Mail, Safari and other apps' data.",
            Permission::AppManagement => "Update and delete apps, like Homebrew upgrading casks.",
            Permission::LocalNetwork => "Reach devices on your network: ssh, local servers and other Macs.",
            Permission::DeveloperTools => "Run what you build in the terminal without macOS checking it first.",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Permission::FullDiskAccess => IconName::HardDrive,
            Permission::AppManagement => IconName::Package,
            Permission::LocalNetwork => IconName::Network,
            Permission::DeveloperTools => IconName::Hammer,
        }
    }

    /// Developer Tools trades a security check for speed, so that one's up to you.
    pub fn required(self) -> bool {
        self != Permission::DeveloperTools
    }

    /// Its page in System Settings. Local Network's can't be linked to, so that one opens the
    /// Privacy & Security page it's listed on.
    fn settings_url(self) -> &'static str {
        match self {
            Permission::FullDiskAccess => "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles",
            Permission::AppManagement => "x-apple.systempreferences:com.apple.preference.security?Privacy_AppBundles",
            Permission::LocalNetwork => "x-apple.systempreferences:com.apple.preference.security?Privacy",
            Permission::DeveloperTools => "x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools",
        }
    }
}

pub struct Permissions {
    statuses: HashMap<Permission, Status>,
    watchers: Rc<Cell<usize>>,
    guide: Option<Guide>,
    /// Whether the sheet that asks for them has been up, which it is once per launch, not per window.
    onboarded: bool,
    _poll: Task<()>,
}

struct GlobalPermissions(Entity<Permissions>);

impl Global for GlobalPermissions {}

/// Keeps the statuses refreshing every second or so while it's alive, for while they're on screen
/// and you're flipping switches in System Settings.
pub struct Watch(Rc<Cell<usize>>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

pub fn init(cx: &mut App) {
    let permissions = cx.new(Permissions::new);
    cx.set_global(GlobalPermissions(permissions));
}

impl Permissions {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalPermissions>().0.clone()
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let watchers = Rc::new(Cell::new(0));
        let poll = cx.spawn({
            let watchers = watchers.clone();
            async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(1200)).await;
                    if watchers.get() > 0 && this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                        break;
                    }
                }
            }
        });
        let mut this = Self { statuses: HashMap::new(), watchers, guide: None, onboarded: false, _poll: poll };
        this.refresh(cx);
        this
    }

    pub fn status(&self, permission: Permission) -> Status {
        self.statuses.get(&permission).copied().unwrap_or(Status::Unknown)
    }

    /// Whether the statuses have been checked yet.
    pub fn checked(&self) -> bool {
        !self.statuses.is_empty()
    }

    /// Required permissions that aren't granted.
    pub fn missing(&self) -> usize {
        Permission::ALL.iter().filter(|p| p.required() && self.status(**p) != Status::Granted).count()
    }

    /// Keeps them refreshing for the sheet that asks for them, the first time it's asked for while a
    /// required one is off.
    pub fn onboard(&mut self) -> Option<Watch> {
        if !self.checked() || self.missing() == 0 || self.onboarded {
            return None;
        }
        self.onboarded = true;
        Some(self.watch())
    }

    pub fn watch(&self) -> Watch {
        self.watchers.set(self.watchers.get() + 1);
        Watch(self.watchers.clone())
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let check = cx.background_spawn(async { check() });
        cx.spawn(async move |this, cx| {
            let statuses = check.await;
            this.update(cx, |this, cx| {
                if this.statuses != statuses {
                    this.statuses = statuses;
                    if let Some(guide) = &mut this.guide {
                        let statuses = &this.statuses;
                        if !guide.advance(|permission| statuses.get(&permission) == Some(&Status::Granted), cx) {
                            this.guide = None;
                            cx.activate(true);
                        }
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks for the ones of `permissions` that aren't allowed by guiding you through System
    /// Settings. Local Network's own prompt has come up already, when Mu looked for file servers at
    /// launch.
    pub fn allow(&mut self, permissions: &[Permission], cx: &mut Context<Self>) {
        let steps = permissions.iter().copied().filter(|&permission| self.status(permission) != Status::Granted);
        self.guide = Guide::new(steps.collect(), self.watch(), cx);
        self.refresh(cx);
    }
}

fn check() -> HashMap<Permission, Status> {
    HashMap::from([
        (Permission::FullDiskAccess, granted("kTCCServiceSystemPolicyAllFiles")),
        (Permission::AppManagement, granted("kTCCServiceSystemPolicyAppBundles")),
        (Permission::LocalNetwork, local_network()),
        (Permission::DeveloperTools, granted("kTCCServiceDeveloperTool")),
    ])
}

/// Asks the privacy daemon whether System Settings allows Mu `service`, the way apps check before
/// prompting. Its databases can't be read directly: App Management's is the user's own, which not
/// even Full Disk Access opens.
fn granted(service: &str) -> Status {
    type Preflight = unsafe extern "C" fn(&CFString, *const c_void) -> i32;
    static PREFLIGHT: LazyLock<Preflight> = LazyLock::new(|| unsafe {
        let tcc = libc::dlopen(c"/System/Library/PrivateFrameworks/TCC.framework/TCC".as_ptr(), libc::RTLD_NOW);
        mem::transmute::<*mut c_void, Preflight>(libc::dlsym(tcc, c"TCCAccessPreflight".as_ptr()))
    });
    // 0 is allowed, 1 turned off and 2 never set.
    match unsafe { PREFLIGHT(&CFString::from_str(service), ptr::null()) } {
        0 => Status::Granted,
        _ => Status::Missing,
    }
}

/// Sends one byte to mDNS's multicast group, which is on every local network. Turned down, that
/// fails with "no route to host". Asked for the first time, it brings up the system's prompt.
fn local_network() -> Status {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else { return Status::Unknown };
    match socket.send_to(&[0], "224.0.0.251:5353") {
        Ok(_) => Status::Granted,
        Err(error) if error.raw_os_error() == Some(libc::EHOSTUNREACH) => Status::Missing,
        Err(_) => Status::Unknown,
    }
}

/// Why to allow them all now, where they're listed.
pub const ALLOW_ONCE: &str = "Allow these once and Mu won't stop to ask later.";

/// Walks you through every permission that isn't allowed yet.
pub fn allow_all_button(cx: &App) -> Button {
    let permissions = Permissions::global(cx);
    Button::new("allow-all")
        .primary()
        .label("Allow All")
        .on_click(move |_, _, cx| permissions.update(cx, |p, cx| p.allow(&Permission::ALL, cx)))
}

/// The permissions as a list, each with what it's for and a way to grant it.
pub fn render_list(cx: &App) -> impl IntoElement + use<> {
    let permissions = Permissions::global(cx);
    let state = permissions.read(cx);
    let theme = cx.theme();
    let rows = Permission::ALL.into_iter().enumerate().map(|(ix, permission)| {
        let status = state.status(permission);
        let action = match status {
            Status::Granted => h_flex()
                .gap_1()
                .text_sm()
                .text_color(theme.success)
                .child(Icon::new(IconName::CircleCheck).small())
                .child("Allowed")
                .into_any_element(),
            _ => {
                let permissions = permissions.clone();
                Button::new(permission.name())
                    .small()
                    .when(permission.required(), |button| button.primary())
                    .label("Allow")
                    .on_click(move |_, _, cx| permissions.update(cx, |p, cx| p.allow(&[permission], cx)))
                    .into_any_element()
            }
        };
        // Short of room, the button goes under the rest.
        h_flex()
            .flex_wrap()
            .gap_3()
            .px_4()
            .py_3()
            .when(ix > 0, |row| row.border_t_1().border_color(theme.border))
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(160.))
                    .gap_3()
                    .child(Icon::new(permission.icon()).size(px(20.)).flex_none().text_color(theme.muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_x_2()
                                    .child(div().font_medium().child(permission.name()))
                                    .when(!permission.required(), |row| {
                                        row.child(div().text_xs().text_color(theme.muted_foreground).child("Optional"))
                                    }),
                            )
                            .child(div().text_sm().text_color(theme.muted_foreground).child(permission.reason())),
                    ),
            )
            .child(div().flex_none().ml_auto().child(action))
    });
    v_flex().rounded(theme.radius_lg).border_1().border_color(theme.border).children(rows.collect::<Vec<_>>())
}

/// The sheet over a window that asks for the permissions at launch. `close` puts it away.
pub fn render_onboarding(close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> Deferred {
    let theme = cx.theme();
    let done = Permissions::global(cx).read(cx).missing() == 0;
    deferred(
        div()
            .id("onboarding")
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .child(
                v_flex()
                    .w(px(600.))
                    .p_5()
                    .gap_4()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_lg().font_semibold().child("Before you start"))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(ALLOW_ONCE)),
                    )
                    .child(render_list(cx))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("onboarding-done")
                                    .when(done, |button| button.primary())
                                    .label(if done { "Done" } else { "Later" })
                                    .on_click(close),
                            )
                            .when(!done, |row| row.child(allow_all_button(cx))),
                    ),
            ),
    )
    .with_priority(2)
}

APP := dist/Mu.app
# A release sets it from its tag.
VERSION ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# Signed ad hoc, unless given a Developer ID, as a release is.
DEVELOPER_ID ?= -

.PHONY: app release icon run test clean

app:
	cargo build --release
	rm -rf $(APP)
	mkdir -p $(APP)/Contents/MacOS $(APP)/Contents/Resources
	cp target/release/mu $(APP)/Contents/MacOS/
	cp assets/icon/AppIcon.icns assets/icon/Assets.car $(APP)/Contents/Resources/
	sed 's/VERSION/$(VERSION)/g' assets/Info.plist > $(APP)/Contents/Info.plist
	codesign --force --sign "$(DEVELOPER_ID)" --options runtime --timestamp $(APP)
	codesign --verify --strict $(APP)

# What a release ships, dist/Mu.zip: notarized by Apple with the ticket stapled on, so it opens on
# any Mac without a Gatekeeper prompt. The notary service takes a zip too, before the ticket goes on.
# Signs in to it with APPLE_ID, APPLE_APP_SPECIFIC_PASSWORD and APPLE_TEAM_ID.
release: app
	rm -f dist/Mu.zip && ditto -c -k --keepParent $(APP) dist/Mu.zip
	xcrun notarytool submit dist/Mu.zip --apple-id "$$APPLE_ID" --password "$$APPLE_APP_SPECIFIC_PASSWORD" --team-id "$$APPLE_TEAM_ID" --wait
	xcrun stapler staple $(APP)
	rm dist/Mu.zip && ditto -c -k --keepParent $(APP) dist/Mu.zip

# Compiles AppIcon.icon (an Icon Composer document) into the Liquid Glass Assets.car and an
# AppIcon.icns, and renders it as assets/icon.png for tools that read a plain picture, like sm and
# T3 Code. All are committed, so building the app doesn't need full Xcode. This does.
# actool takes relative paths from somewhere other than here, so they're absolute.
XCODE ?= /Applications/Xcode.app/Contents/Developer
ICTOOL = $(XCODE)/../Applications/Icon Composer.app/Contents/Executables/ictool

icon:
	DEVELOPER_DIR=$(XCODE) xcrun actool $(CURDIR)/assets/icon/AppIcon.icon --compile $(CURDIR)/assets/icon --app-icon AppIcon \
		--platform macosx --target-device mac --minimum-deployment-target 27.0 \
		--output-partial-info-plist /dev/null
	# 206 of 256, in the margin macOS gives an app icon.
	"$(ICTOOL)" $(CURDIR)/assets/icon/AppIcon.icon --export-image --output-file $(CURDIR)/assets/icon.png \
		--platform macOS --rendition Default --width 206 --height 206 --scale 1
	sips --padToHeightWidth 256 256 assets/icon.png >/dev/null

run:
	cargo run

test:
	cargo test --workspace

clean:
	cargo clean
	rm -rf dist

//! The I/O Registry, where macOS's drivers publish what they measure: how busy the GPU is and
//! which processes use it, how much the disks have read and written, and the battery's charge.

use std::{
    ffi::{CStr, c_char, c_void},
    ptr::NonNull,
};

use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iterator: *mut u32) -> i32;
    fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, iterator: *mut u32) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: &CFString,
        allocator: *const c_void,
        options: u32,
    ) -> Option<NonNull<CFType>>;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
}

/// An object in the registry, let go of when dropped.
pub struct Entry(u32);

impl Drop for Entry {
    fn drop(&mut self) {
        unsafe { IOObjectRelease(self.0) };
    }
}

/// The services of `class`, like `IOAccelerator` for the GPU.
pub fn services(class: &CStr) -> Vec<Entry> {
    let mut iterator = 0;
    // The matching dictionary is the call's to release.
    match unsafe { IOServiceGetMatchingServices(0, IOServiceMatching(class.as_ptr()), &mut iterator) } {
        0 => entries(iterator),
        _ => Vec::new(),
    }
}

fn entries(iterator: u32) -> Vec<Entry> {
    let entries = std::iter::from_fn(|| match unsafe { IOIteratorNext(iterator) } {
        0 => None,
        entry => Some(Entry(entry)),
    })
    .collect();
    unsafe { IOObjectRelease(iterator) };
    entries
}

impl Entry {
    /// What's attached below it, like the GPU's clients.
    pub fn children(&self) -> Vec<Entry> {
        let mut iterator = 0;
        match unsafe { IORegistryEntryGetChildIterator(self.0, c"IOService".as_ptr(), &mut iterator) } {
            0 => entries(iterator),
            _ => Vec::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let value = unsafe { IORegistryEntryCreateCFProperty(self.0, &CFString::from_str(key), std::ptr::null(), 0) }?;
        Some(Value(unsafe { CFRetained::from_raw(value) }))
    }
}

/// A property's value: a number, string, flag, list or dictionary of more values.
pub struct Value(CFRetained<CFType>);

impl Value {
    pub fn int(&self) -> Option<i64> {
        self.0.downcast_ref::<CFNumber>()?.as_i64()
    }

    pub fn bool(&self) -> Option<bool> {
        self.0.downcast_ref::<CFBoolean>().map(CFBoolean::as_bool)
    }

    pub fn string(&self) -> Option<String> {
        self.0.downcast_ref::<CFString>().map(CFString::to_string)
    }

    /// The value under `key`, when this is a dictionary.
    pub fn get(&self, key: &str) -> Option<Value> {
        let dictionary = unsafe { self.0.downcast_ref::<CFDictionary>()?.cast_unchecked::<CFString, CFType>() };
        dictionary.get(&CFString::from_str(key)).map(Value)
    }

    /// The values in it, when this is a list.
    pub fn items(&self) -> Vec<Value> {
        let Some(array) = self.0.downcast_ref::<CFArray>() else { return Vec::new() };
        unsafe { array.cast_unchecked::<CFType>() }.iter().map(Value).collect()
    }
}

//! Minimal raw-COM UI Automation bindings used by `media_ext::librewolf_timeline`.
//!
//! The workspace `windows` crate (owned by the orchestrator) does not enable the
//! `Win32_UI_Accessibility` feature, so the strongly-typed `IUIAutomation*` interfaces are not
//! available. These vtables are the exact ABI layout of `uiautomationclient.h` (verified against
//! the generated `windows` 0.61 bindings), which lets the production reader call the same COM
//! objects without changing any `Cargo.toml`. When the feature is enabled later, `media_ext` can
//! switch to the typed bindings; this module stays private and warning-clean.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr;

use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, GUID};

pub type ComPtr = *mut c_void;

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoCreateInstance(
        clsid: *const GUID,
        outer: *mut c_void,
        cls_context: u32,
        iid: *const GUID,
        out: *mut ComPtr,
    ) -> i32;
}

// CLSID / IID values from uiautomationclient.h.
const CLSID_CUI_AUTOMATION: GUID = GUID::from_u128(0xff48dba4_60ef_4201_aa87_54103ee594f5);
const IID_IUI_AUTOMATION: GUID = GUID::from_u128(0x35c5905a_5a69_499c_a1c7_bfce30cc556d);
const IID_IUI_AUTOMATION_RANGE_VALUE_PATTERN: GUID =
    GUID::from_u128(0x59213f4f_7346_49e5_b120_80555987a148);
const IID_IUI_AUTOMATION_VALUE_PATTERN: GUID =
    GUID::from_u128(0xa94cd8b1_0844_4cd6_9d2d_640537ab39e9);

pub const TREE_SCOPE_DESCENDANTS: i32 = 4;
pub const UIA_CONTROL_TYPE_PROPERTY_ID: i32 = 30003;
pub const UIA_AUTOMATION_ID_PROPERTY_ID: i32 = 30011;
pub const UIA_DOCUMENT_CONTROL_TYPE_ID: i32 = 50030;
pub const UIA_SLIDER_CONTROL_TYPE_ID: i32 = 50015;
pub const UIA_RANGE_VALUE_PATTERN_ID: i32 = 10003;
pub const UIA_VALUE_PATTERN_ID: i32 = 10002;

#[repr(C)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(ComPtr, *const GUID, *mut ComPtr) -> i32,
    add_ref: unsafe extern "system" fn(ComPtr) -> u32,
    release: unsafe extern "system" fn(ComPtr) -> u32,
}

/// IUIAutomation vtable (0-based slots, `IUnknown` occupies 0..2).
///
/// SDK order: 3 `CompareElements`, 4 `CompareRuntimeIds`, 5 `GetRootElement`,
/// 6 `ElementFromHandle`, …, 21 `CreateTrueCondition`, 22 `CreateFalseCondition`,
/// 23 `CreatePropertyCondition`. The previously used slots (5 / 22) pointed at
/// `GetRootElement` / `CreateFalseCondition`, which wrote through the caller's
/// arguments as if they were COM out-parameters and could crash the process.
#[repr(C)]
struct AutomationVtbl {
    base: UnknownVtbl,
    _p3: usize,
    _p4: usize,
    get_root_element: unsafe extern "system" fn(ComPtr, *mut ComPtr) -> i32,
    element_from_handle: unsafe extern "system" fn(ComPtr, ComPtr, *mut ComPtr) -> i32,
    _p7: usize,
    _p8: usize,
    _p9: usize,
    _p10: usize,
    _p11: usize,
    _p12: usize,
    _p13: usize,
    _p14: usize,
    _p15: usize,
    _p16: usize,
    _p17: usize,
    _p18: usize,
    _p19: usize,
    _p20: usize,
    _p21: usize,
    _p22: usize,
    create_property_condition: unsafe extern "system" fn(ComPtr, i32, VARIANT, *mut ComPtr) -> i32,
}

/// IUIAutomationElement vtable (0-based slots, `IUnknown` occupies 0..2).
///
/// SDK order: 3 `SetFocus`, 4 `GetRuntimeId`, 5 `FindFirst`, 6 `FindAll`,
/// 14 `GetCurrentPatternAs`, 15 `GetCachedPatternAs`, 16 `GetCurrentPattern`,
/// 20 `CurrentProcessId`, 21 `CurrentControlType`, 22 `CurrentLocalizedControlType`,
/// 23 `CurrentName`. Slots 5/15/21 previously called `FindFirst` /
/// `GetCachedPatternAs` / `CurrentControlType`, producing empty results or a
/// bogus BSTR pointer.
#[repr(C)]
struct ElementVtbl {
    base: UnknownVtbl,
    _p3: usize,
    _p4: usize,
    _p5: usize,
    find_all: unsafe extern "system" fn(ComPtr, i32, ComPtr, *mut ComPtr) -> i32,
    _p7: usize,
    _p8: usize,
    _p9: usize,
    _p10: usize,
    _p11: usize,
    _p12: usize,
    _p13: usize,
    _p14: usize,
    _p15: usize,
    get_current_pattern: unsafe extern "system" fn(ComPtr, i32, *mut ComPtr) -> i32,
    _p17: usize,
    _p18: usize,
    _p19: usize,
    _p20: usize,
    _p21: usize,
    _p22: usize,
    current_name: unsafe extern "system" fn(ComPtr, *mut *mut u16) -> i32,
}

#[repr(C)]
struct ArrayVtbl {
    base: UnknownVtbl,
    length: unsafe extern "system" fn(ComPtr, *mut i32) -> i32,
    get_element: unsafe extern "system" fn(ComPtr, i32, *mut ComPtr) -> i32,
}

/// `IUIAutomationRangeValuePattern`: 3 `SetValue`, 4 `CurrentValue`,
/// 5 `CurrentIsReadOnly`, 6 `CurrentMaximum`, 7 `CurrentMinimum`.
#[repr(C)]
struct RangeValueVtbl {
    base: UnknownVtbl,
    _set_value: usize,
    value: unsafe extern "system" fn(ComPtr, *mut f64) -> i32,
    _is_read_only: usize,
    maximum: unsafe extern "system" fn(ComPtr, *mut f64) -> i32,
    minimum: unsafe extern "system" fn(ComPtr, *mut f64) -> i32,
}

/// `IUIAutomationValuePattern`: 3 `SetValue`, 4 `CurrentValue`.
#[repr(C)]
struct ValueVtbl {
    base: UnknownVtbl,
    _set_value: usize,
    value: unsafe extern "system" fn(ComPtr, *mut *mut u16) -> i32,
}

// Compile-time ABI guards: every vtable entry is pointer-sized, so a wrong
// placeholder count would shift every subsequent method call at runtime.
const _: () = assert!(size_of::<AutomationVtbl>() == 24 * size_of::<usize>());
const _: () = assert!(size_of::<ElementVtbl>() == 24 * size_of::<usize>());
const _: () = assert!(size_of::<ArrayVtbl>() == 5 * size_of::<usize>());
const _: () = assert!(size_of::<RangeValueVtbl>() == usize::BITS as usize);
const _: () = assert!(size_of::<ValueVtbl>() == 5 * size_of::<usize>());

unsafe fn vtable<T>(ptr: ComPtr) -> &'static T {
    // SAFETY: `ptr` is a live COM object whose first field points at its vtable.
    unsafe { &*(*(ptr as *const *const T)) }
}

/// Reference-counted COM pointer, released exactly once on Drop.
pub struct Com(ComPtr, PhantomData<u8>);

impl Com {
    fn from_ptr(ptr: ComPtr) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr, PhantomData))
        }
    }

    pub fn as_ptr(&self) -> ComPtr {
        self.0
    }

    /// Obtains another interface from the same object (`QueryInterface`).
    fn query(&self, iid: &GUID) -> Option<Self> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: `self.0` is live and `out` receives one reference on success.
        let hr = unsafe { (vtable::<UnknownVtbl>(self.0).query_interface)(self.0, iid, &mut out) };
        if hr >= 0 { Self::from_ptr(out) } else { None }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a live reference acquired via CoCreateInstance / QueryInterface.
            unsafe {
                (vtable::<UnknownVtbl>(self.0).release)(self.0);
            }
        }
    }
}

/// Owns `IUIAutomation` created by `CUIAutomation`.
pub struct Automation(Com);

impl Automation {
    pub fn new() -> Option<Self> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: all pointers are valid out-parameters; COM is initialized by the caller.
        let hr = unsafe {
            CoCreateInstance(
                &CLSID_CUI_AUTOMATION,
                ptr::null_mut(),
                1, // CLSCTX_INPROC_SERVER
                &IID_IUI_AUTOMATION,
                &mut out,
            )
        };
        if hr >= 0 {
            Com::from_ptr(out).map(Self)
        } else {
            None
        }
    }

    /// `IUIAutomation::GetRootElement` (slot 5) — the desktop root element.
    pub fn root_element(&self) -> Option<Com> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: `self.0.0` is live; `out` receives one reference on success.
        let hr = unsafe {
            (vtable::<AutomationVtbl>(self.0.as_ptr()).get_root_element)(self.0.as_ptr(), &mut out)
        };
        if hr >= 0 { Com::from_ptr(out) } else { None }
    }

    pub fn element_from_handle(&self, hwnd: ComPtr) -> Option<Com> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: `self.0.0` is live; `hwnd` is a live window handle from EnumWindows.
        let hr = unsafe {
            (vtable::<AutomationVtbl>(self.0.as_ptr()).element_from_handle)(
                self.0.as_ptr(),
                hwnd,
                &mut out,
            )
        };
        if hr >= 0 { Com::from_ptr(out) } else { None }
    }

    pub fn property_condition_i32(&self, property_id: i32, value: i32) -> Option<Com> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: the VARIANT is passed by value per COM ABI and released after the call.
        let hr = unsafe {
            (vtable::<AutomationVtbl>(self.0.as_ptr()).create_property_condition)(
                self.0.as_ptr(),
                property_id,
                VARIANT::from(value),
                &mut out,
            )
        };
        if hr >= 0 { Com::from_ptr(out) } else { None }
    }

    pub fn property_condition_text(&self, property_id: i32, value: &str) -> Option<Com> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: the BSTR-backed VARIANT is passed by value and freed after the call.
        let hr = unsafe {
            (vtable::<AutomationVtbl>(self.0.as_ptr()).create_property_condition)(
                self.0.as_ptr(),
                property_id,
                VARIANT::from(value),
                &mut out,
            )
        };
        if hr >= 0 { Com::from_ptr(out) } else { None }
    }
}

/// Owns an `IUIAutomationElement`.
pub struct Element(Com);

impl Element {
    pub fn from_com(com: Com) -> Self {
        Self(com)
    }

    pub fn find_all(&self, scope: i32, condition: &Com) -> Option<Com> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: both interfaces are live for the duration of the call.
        let hr = unsafe {
            (vtable::<ElementVtbl>(self.0.as_ptr()).find_all)(
                self.0.as_ptr(),
                scope,
                condition.as_ptr(),
                &mut out,
            )
        };
        if hr >= 0 { Com::from_ptr(out) } else { None }
    }

    pub fn current_name(&self) -> Option<String> {
        let mut raw: *mut u16 = ptr::null_mut();
        // SAFETY: `raw` is a live BSTR out-parameter freed by `BSTR::from_raw` below.
        let hr = unsafe {
            (vtable::<ElementVtbl>(self.0.as_ptr()).current_name)(self.0.as_ptr(), &mut raw)
        };
        if hr < 0 || raw.is_null() {
            return None;
        }
        // SAFETY: UIA owns and returns a freshly allocated BSTR we take ownership of here.
        let text = unsafe { BSTR::from_raw(raw as *const u16) }.to_string();
        (!text.is_empty()).then_some(text)
    }

    fn pattern(&self, pattern_id: i32, iid: &GUID) -> Option<Com> {
        let mut out: ComPtr = ptr::null_mut();
        // SAFETY: `self.0` is live and `out` receives an IUnknown pattern reference.
        let hr = unsafe {
            (vtable::<ElementVtbl>(self.0.as_ptr()).get_current_pattern)(
                self.0.as_ptr(),
                pattern_id,
                &mut out,
            )
        };
        if hr < 0 {
            return None;
        }
        let unknown = Com::from_ptr(out)?;
        unknown.query(iid)
    }

    /// ValuePattern.CurrentValue — the address bar of a document or label of a slider.
    pub fn value_text(&self) -> Option<String> {
        let pattern = self.pattern(UIA_VALUE_PATTERN_ID, &IID_IUI_AUTOMATION_VALUE_PATTERN)?;
        let mut raw: *mut u16 = ptr::null_mut();
        // SAFETY: the pattern is live; the BSTR out-parameter is freed below.
        let hr =
            unsafe { (vtable::<ValueVtbl>(pattern.as_ptr()).value)(pattern.as_ptr(), &mut raw) };
        if hr < 0 || raw.is_null() {
            return None;
        }
        // SAFETY: ownership of the returned BSTR transfers to `BSTR`.
        let text = unsafe { BSTR::from_raw(raw as *const u16) }.to_string();
        (!text.is_empty()).then_some(text)
    }

    /// RangeValuePattern `(minimum, maximum, value)`.
    pub fn range_value(&self) -> Option<(f64, f64, f64)> {
        let pattern = self.pattern(
            UIA_RANGE_VALUE_PATTERN_ID,
            &IID_IUI_AUTOMATION_RANGE_VALUE_PATTERN,
        )?;
        let vtbl = unsafe { vtable::<RangeValueVtbl>(pattern.as_ptr()) };
        let mut value = 0.0;
        let mut maximum = 0.0;
        let mut minimum = 0.0;
        // SAFETY: all out-parameters are live and the pattern interface is live.
        unsafe {
            if (vtbl.value)(pattern.as_ptr(), &mut value) < 0
                || (vtbl.maximum)(pattern.as_ptr(), &mut maximum) < 0
                || (vtbl.minimum)(pattern.as_ptr(), &mut minimum) < 0
            {
                return None;
            }
        }
        Some((minimum, maximum, value))
    }
}

/// Marker guard: keeps the `ElementArray` COM reference alive while iterating.
pub struct ElementArray(Com);

impl ElementArray {
    pub fn from_com(com: Com) -> Self {
        Self(com)
    }

    pub fn elements(&self) -> Option<Vec<Element>> {
        let vtbl = unsafe { vtable::<ArrayVtbl>(self.0.as_ptr()) };
        let mut count = 0i32;
        // SAFETY: the array is live and `count` is a live out-parameter.
        if unsafe { (vtbl.length)(self.0.as_ptr(), &mut count) } < 0 || count < 0 {
            return None;
        }
        let mut elements = Vec::with_capacity(count.min(256) as usize);
        for index in 0..count.min(256) {
            let mut raw: ComPtr = ptr::null_mut();
            // SAFETY: `raw` receives one reference and is released on Drop.
            if unsafe { (vtbl.get_element)(self.0.as_ptr(), index, &mut raw) } >= 0
                && let Some(com) = Com::from_ptr(raw)
            {
                elements.push(Element(com));
            }
        }
        Some(elements)
    }
}

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    /// Static guard for the ABI offsets. The ignored COM test below exercises the
    /// real objects; this one catches accidental placeholder drift on every build.
    #[test]
    fn vtable_slots_match_sdk_method_order() {
        assert_eq!(
            offset_of!(AutomationVtbl, get_root_element),
            5 * size_of::<usize>()
        );
        assert_eq!(
            offset_of!(AutomationVtbl, element_from_handle),
            6 * size_of::<usize>()
        );
        assert_eq!(
            offset_of!(AutomationVtbl, create_property_condition),
            23 * size_of::<usize>()
        );
        assert_eq!(offset_of!(ElementVtbl, find_all), 6 * size_of::<usize>());
        assert_eq!(
            offset_of!(ElementVtbl, get_current_pattern),
            16 * size_of::<usize>()
        );
        assert_eq!(
            offset_of!(ElementVtbl, current_name),
            23 * size_of::<usize>()
        );
        assert_eq!(offset_of!(RangeValueVtbl, value), 4 * size_of::<usize>());
        assert_eq!(offset_of!(RangeValueVtbl, maximum), 6 * size_of::<usize>());
        assert_eq!(offset_of!(RangeValueVtbl, minimum), 7 * size_of::<usize>());
    }

    /// Real-object smoke test. Ignored by default because it needs an interactive
    /// desktop and a working UIA service; run explicitly with
    /// `cargo test -p opentaskbar-app uia_raw::tests::com_smoke -- --ignored`.
    #[test]
    #[ignore = "requires an interactive desktop UIA session"]
    fn com_smoke_test_root_and_property_condition() {
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        {
            let automation = Automation::new().expect("CUIAutomation must be creatable");
            let condition = automation
                .property_condition_i32(UIA_CONTROL_TYPE_PROPERTY_ID, UIA_DOCUMENT_CONTROL_TYPE_ID)
                .expect("CreatePropertyCondition must return a condition");
            let root = Element::from_com(automation.root_element().expect("root element"));
            // CurrentName returns a BSTR; a bogus slot would either fail or crash here.
            let _ = root.current_name();
            if let Some(com) =
                automation.element_from_handle(unsafe { GetDesktopWindow() } as ComPtr)
            {
                let _ = Element::from_com(com).current_name();
            }
            drop(condition);
        }
        if initialized {
            unsafe { CoUninitialize() };
        }
    }
}

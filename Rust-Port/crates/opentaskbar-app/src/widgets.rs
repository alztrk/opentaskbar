//! Lightweight native Win32 telemetry providers for dock widgets (Clock, CPU/RAM, Recycle Bin).
//! Zero external crates, zero allocations on idle, and non-blocking reads.

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::SystemInformation::{
    GetLocalTime, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows_sys::Win32::System::Threading::GetSystemTimes;
use windows_sys::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW, SHQUERYRBINFO,
    SHQueryRecycleBinW,
};

fn ft_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

#[derive(Default)]
pub struct SystemStatsSampler {
    prev_idle: u64,
    prev_kernel: u64,
    prev_user: u64,
}

impl SystemStatsSampler {
    pub fn sample(&mut self) -> (u32, u32) {
        let cpu = self.sample_cpu();
        let ram = sample_ram_percent();
        (cpu, ram)
    }

    fn sample_cpu(&mut self) -> u32 {
        let mut idle = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: querying OS system CPU times into stack structs.
        let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        if ok == 0 {
            return 0;
        }
        let idle_v = ft_to_u64(idle);
        let kernel_v = ft_to_u64(kernel);
        let user_v = ft_to_u64(user);

        let pct = if self.prev_kernel != 0 || self.prev_user != 0 {
            let d_idle = idle_v.saturating_sub(self.prev_idle);
            let d_kernel = kernel_v.saturating_sub(self.prev_kernel);
            let d_user = user_v.saturating_sub(self.prev_user);
            let total = d_kernel.saturating_add(d_user);
            if total > 0 {
                let busy = total.saturating_sub(d_idle);
                ((busy.saturating_mul(100)) / total).min(100) as u32
            } else {
                0
            }
        } else {
            0
        };

        self.prev_idle = idle_v;
        self.prev_kernel = kernel_v;
        self.prev_user = user_v;
        pct
    }
}

fn sample_ram_percent() -> u32 {
    let mut mem: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    mem.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: properly sized MEMORYSTATUSEX.
    if unsafe { GlobalMemoryStatusEx(&mut mem) } != 0 {
        mem.dwMemoryLoad.min(100)
    } else {
        0
    }
}

/// Returns `(HH:MM, dd.MM.yyyy)` in the local system timezone.
pub fn local_clock_strings() -> (String, String) {
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: reading local wall clock into stack struct.
    unsafe {
        GetLocalTime(&mut st);
    }
    let time = format!("{:02}:{:02}", st.wHour, st.wMinute);
    let date = format!("{:02}.{:02}.{}", st.wDay, st.wMonth, st.wYear);
    (time, date)
}

pub fn is_recycle_bin_non_empty() -> bool {
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHQUERYRBINFO>() as u32;
    // SAFETY: querying default Recycle Bin item count.
    let hr = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    hr == 0 && info.i64NumItems > 0
}

pub fn empty_recycle_bin() {
    std::thread::Builder::new()
        .name("opentaskbar-empty-bin".into())
        .spawn(|| unsafe {
            let _ = SHEmptyRecycleBinW(
                std::ptr::null_mut(),
                std::ptr::null(),
                SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
            );
        })
        .ok();
}

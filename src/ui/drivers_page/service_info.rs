//! What the Services page's side panel says about the selected service: the description from
//! the Service Control Manager and the company of the image. Both are read on first use and kept.

use std::collections::HashMap;

/// Facts about one service that the service list itself does not carry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServiceInfo {
    /// The text shown in services.msc, when the service has one.
    pub description: Option<String>,
    /// CompanyName of the service image.
    pub company: String,
}

/// [`ServiceInfo`] per service name, read the first time the service is looked at.
#[derive(Default)]
pub struct InfoCache {
    map: HashMap<String, ServiceInfo>,
}

impl InfoCache {
    /// The facts of service `name` whose image is `path`.
    pub fn get(&mut self, name: &str, path: &str) -> &ServiceInfo {
        self.map.entry(name.to_lowercase()).or_insert_with(|| ServiceInfo {
            description: description(name),
            company: crate::pro::drivers::file_version(path).map(|v| v.company).unwrap_or_default(),
        })
    }
}

/// `SERVICE_CONFIG_DESCRIPTION` of service `name`.
#[cfg(windows)]
fn description(name: &str) -> Option<String> {
    use crate::pro::winutil::wide;
    use windows::core::PCWSTR;
    use windows::Win32::System::Services::{CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceConfig2W, SC_HANDLE, SERVICE_CONFIG_DESCRIPTION, SERVICE_DESCRIPTIONW};

    struct H(SC_HANDLE);
    impl Drop for H {
        fn drop(&mut self) {
            // SAFETY: handle came from OpenSCManagerW/OpenServiceW.
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    const SC_MANAGER_CONNECT: u32 = 0x1;
    const SERVICE_QUERY_CONFIG: u32 = 0x1;
    // SAFETY: local SCM, default database.
    let scm = H(unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }.ok()?);
    let wname = wide(name);
    // SAFETY: valid, NUL-terminated service name.
    let svc = H(unsafe { OpenServiceW(scm.0, PCWSTR(wname.as_ptr()), SERVICE_QUERY_CONFIG) }.ok()?);
    let mut need = 0u32;
    // SAFETY: size query.
    let _ = unsafe { QueryServiceConfig2W(svc.0, SERVICE_CONFIG_DESCRIPTION, None, &mut need) };
    if need == 0 || need > 1 << 20 {
        return None;
    }
    let mut buf = vec![0u64; need as usize / 8 + 1];
    // SAFETY: the buffer is at least `need` bytes and 8-byte aligned.
    let bytes: &mut [u8] = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
    // SAFETY: as above.
    unsafe { QueryServiceConfig2W(svc.0, SERVICE_CONFIG_DESCRIPTION, Some(bytes), &mut need) }.ok()?;
    // SAFETY: the call wrote a SERVICE_DESCRIPTIONW at the start of the buffer; its string lives in the same buffer.
    let text = unsafe {
        let d = &*(buf.as_ptr() as *const SERVICE_DESCRIPTIONW);
        if d.lpDescription.is_null() {
            return None;
        }
        d.lpDescription.to_string().ok()?
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(windows))]
fn description(_name: &str) -> Option<String> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Windows ships a description for the DNS Client service.
    #[test]
    fn reads_a_description() {
        let d = description("Dnscache").expect("Dnscache has a description");
        assert!(!d.starts_with('@'), "indirect strings come resolved: {d}");
        let mut cache = InfoCache::default();
        assert_eq!(cache.get("Dnscache", "").description.as_deref(), Some(d.as_str()));
        assert_eq!(description("no-such-service-tmop"), None);
    }
}

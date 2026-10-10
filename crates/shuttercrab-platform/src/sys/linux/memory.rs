//! This process's memory on Linux, from `/proc/self/status`: its anonymous
//! resident memory, nearest to Windows' private bytes. Graphics memory is
//! not counted yet.

use crate::memory::Usage;

/// The usage now.
pub(crate) fn usage() -> Usage {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    Usage {
        private: kilobytes(&status, "RssAnon").unwrap_or(0) * 1024,
        ..Usage::default()
    }
}

/// The value of `field` in `/proc/self/status` text, in kilobytes.
fn kilobytes(status: &str, field: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let value = line.strip_prefix(field)?.strip_prefix(':')?;
        value.trim().strip_suffix("kB")?.trim().parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_kilobyte_fields() {
        let status = [
            "Name:\tshuttercrab",
            "VmRSS:\t  204800 kB",
            "RssAnon:\t  153600 kB",
        ]
        .join("\n");
        assert_eq!(kilobytes(&status, "RssAnon"), Some(153_600));
        assert_eq!(kilobytes(&status, "RssFile"), None);
        // A running process has some.
        assert!(usage().private > 0);
    }
}

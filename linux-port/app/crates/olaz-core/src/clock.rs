//! Wall clock with an optional fixed value (scene renders freeze it so relative times are reproducible).

use std::sync::atomic::{AtomicI64, Ordering};

static FIXED_MS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Freezes [`now_ms`] at `ms` (epoch milliseconds); `None` returns to the system clock.
pub fn set_fixed(ms: Option<i64>) {
    FIXED_MS.store(ms.unwrap_or(i64::MIN), Ordering::SeqCst);
}

/// Epoch milliseconds: the fixed clock when set, the system clock otherwise.
pub fn now_ms() -> i64 {
    let fixed = FIXED_MS.load(Ordering::SeqCst);
    if fixed != i64::MIN {
        return fixed;
    }
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixed_and_system() {
        super::set_fixed(Some(1_700_000_000_000));
        assert_eq!(super::now_ms(), 1_700_000_000_000);
        super::set_fixed(None);
        assert!(super::now_ms() > 1_700_000_000_000);
    }
}

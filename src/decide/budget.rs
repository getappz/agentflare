//! Per-run admission limits, inspired by Stanley's bounded frame executor.
//! Bytes are exact serialized input sizes; they are not tokenizer estimates.

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_requests: usize,
    pub max_input_bytes: usize,
    pub max_request_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_requests: 48,
            max_input_bytes: 512_000,
            max_request_bytes: 64_000,
        }
    }
}

#[derive(Debug, Default)]
pub struct Budget {
    pub requests: usize,
    pub input_bytes: usize,
    pub reported_input_tokens: u64,
    pub reported_output_tokens: u64,
    limits: Limits,
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// Reserve before transport. Failed attempts retain their reservation:
    /// a provider may have processed a request even if its response was lost.
    pub fn reserve(&mut self, bytes: usize) -> Result<(), &'static str> {
        if bytes > self.limits.max_request_bytes {
            return Err("per-request input byte limit");
        }
        if self.requests >= self.limits.max_requests {
            return Err("request count limit");
        }
        let total = self
            .input_bytes
            .checked_add(bytes)
            .ok_or("input byte limit")?;
        if total > self.limits.max_input_bytes {
            return Err("total input byte limit");
        }
        self.requests += 1;
        self.input_bytes = total;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ceiling_rejects_before_spending_more() {
        let mut budget = Budget::new(Limits {
            max_requests: 1,
            ..Limits::default()
        });
        budget.reserve(100).unwrap();
        assert!(budget.reserve(100).is_err());
        assert_eq!((budget.requests, budget.input_bytes), (1, 100));
    }

    #[test]
    fn oversized_input_does_not_consume_a_reservation() {
        let mut budget = Budget::new(Limits {
            max_input_bytes: 10,
            max_request_bytes: 8,
            ..Limits::default()
        });
        assert!(budget.reserve(9).is_err());
        budget.reserve(6).unwrap();
        assert!(budget.reserve(5).is_err());
        assert_eq!((budget.requests, budget.input_bytes), (1, 6));
    }
}

//! `zebra_scan::service::ScanService` request types.

use std::collections::HashSet;

use crate::BoxError;

/// The maximum number of keys that may be included in a request to the scan service
const MAX_REQUEST_KEYS: usize = 1000;

#[derive(Debug)]
/// Request types for `zebra_scan::service::ScanService`
pub enum Request {
    /// Requests general info about the scanner
    Info,

    /// Submits viewing keys with their optional birth-heights for scanning.
    RegisterKeys(Vec<(String, Option<u32>)>),

    /// Deletes viewing keys and their results from the database.
    DeleteKeys(Vec<String>),

    /// Accept keys and return transaction data
    Results(Vec<String>),

    /// Accept keys and return a channel receiver for transaction data
    SubscribeResults(HashSet<String>),

    /// Clear the results for a set of viewing keys
    ClearResults(Vec<String>),
}

impl Request {
    /// Check that the request data is valid for the request variant
    pub fn check(&self) -> Result<(), BoxError> {
        self.check_num_keys()?;

        Ok(())
    }

    /// Checks that requests which include keys have a valid number of keys.
    fn check_num_keys(&self) -> Result<(), BoxError> {
        match self {
            Request::DeleteKeys(keys) | Request::ClearResults(keys)
                if keys.is_empty() || keys.len() > MAX_REQUEST_KEYS =>
            {
                Err(format!("request must include between 1 and {MAX_REQUEST_KEYS} keys").into())
            }

            _ => Ok(()),
        }
    }
}

#[test]
fn test_check_num_keys() {
    let fake_keys: Vec<_> = std::iter::repeat_n(String::new(), MAX_REQUEST_KEYS + 1).collect();

    let bad_requests = [
        Request::DeleteKeys(vec![]),
        Request::DeleteKeys(fake_keys.clone()),
        Request::ClearResults(vec![]),
        Request::ClearResults(fake_keys),
    ];

    let valid_requests = [
        Request::DeleteKeys(vec![String::new()]),
        Request::ClearResults(vec![String::new()]),
    ];

    for request in bad_requests {
        let error = request.check().expect_err("check should return an error");

        assert_eq!(
            format!("request must include between 1 and {MAX_REQUEST_KEYS} keys"),
            error.to_string(),
            "check_num_keys should return an error because there are too many keys"
        );
    }

    for request in valid_requests {
        request.check().expect("check should return Ok(())");
    }
}

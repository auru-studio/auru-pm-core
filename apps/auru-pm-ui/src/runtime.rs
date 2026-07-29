use std::future::Future;

/// Run a Tokio-dependent future from a non-Tokio executor.
///
/// GPUI owns the desktop event loop and its tasks are not automatically
/// entered into a Tokio runtime. Network futures must cross this boundary
/// explicitly.
pub(crate) fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("background runtime: {error}"))?;

    Ok(runtime.block_on(future))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_future_should_not_need_an_ambient_tokio_runtime() {
        let result = block_on(auru_pm::HttpProvider::probe_health(
            "http://localhost:9",
        ))
        .expect("the bridge should create its own runtime");

        assert!(
            result.is_err(),
            "the closed local port should return a network error"
        );
    }
}

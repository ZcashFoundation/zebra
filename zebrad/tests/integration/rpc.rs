use color_eyre::eyre::{Result, WrapErr};

use zebra_chain::parameters::Network::*;
use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;
use zebra_test::{args, prelude::*};

use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

// Used by metrics_endpoint and tracing_endpoint (feature gated).
#[cfg(any(feature = "prometheus", feature = "filter-reload"))]
use crate::common::config::default_test_config;
#[cfg(any(feature = "prometheus", feature = "filter-reload"))]
use zebra_test::net::random_known_port;

#[tokio::test]
#[cfg(feature = "prometheus")]
async fn metrics_endpoint() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{port}");
    let url = format!("http://{endpoint}");

    // Write a configuration that has metrics endpoint_addr set
    let mut config = default_test_config(&Mainnet);
    config.network.cache_dir = false.into();
    config.metrics.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?.with_timeout(LAUNCH_DELAY);
    child.expect_stdout_line_matches(regex::escape(&format!(
        "Opened metrics endpoint at {endpoint}"
    )))?;

    // Create an http client
    let client = reqwest::Client::builder().timeout(LAUNCH_DELAY).build()?;

    // Test metrics endpoint
    let res = client.get(url).send().await;

    let (res, child) = child.with_timeout(LAUNCH_DELAY).kill_on_error(res)?;
    assert!(res.status().is_success());

    // Get the body of the response
    let body = res.bytes().await;
    let (body, mut child) = child.with_timeout(LAUNCH_DELAY).kill_on_error(body)?;
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.any_output_line_contains(
        "# TYPE zebrad_build_info counter",
        &body,
        "metrics exporter response",
        "the metrics response header",
    )?;
    std::str::from_utf8(&body).expect("unexpected invalid UTF-8 in metrics exporter response");

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;

    Ok(())
}

#[cfg(feature = "filter-reload")]
#[tokio::test]
async fn tracing_endpoint() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let endpoint = format!("127.0.0.1:{port}");
    let url_default = format!("http://{endpoint}");
    let url_filter = format!("{url_default}/filter");

    // Write a configuration that has tracing endpoint_addr option set
    let mut config = default_test_config(&Mainnet);
    config.network.cache_dir = false.into();
    config.tracing.endpoint_addr = Some(endpoint.parse().unwrap());

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?.with_timeout(LAUNCH_DELAY);
    child.expect_stdout_line_matches(regex::escape(&format!(
        "Opened tracing endpoint at {endpoint}"
    )))?;

    // Create an http client
    let client = reqwest::Client::builder().timeout(LAUNCH_DELAY).build()?;

    // Test tracing endpoint
    let res = client.get(url_default).send().await;
    let (res, child) = child.with_timeout(LAUNCH_DELAY).kill_on_error(res)?;
    assert!(res.status().is_success());

    // Get the body of the response
    let body = res.bytes().await;
    let (body, child) = child.with_timeout(LAUNCH_DELAY).kill_on_error(body)?;

    // Set a filter and make sure it was changed
    let post = client.post(&url_filter).body("zebrad=debug").send().await;
    let (_post, child) = child.with_timeout(LAUNCH_DELAY).kill_on_error(post)?;

    let tracing_res = client.get(url_filter).send().await;

    let (tracing_res, child) = child
        .with_timeout(LAUNCH_DELAY)
        .kill_on_error(tracing_res)?;
    assert!(tracing_res.status().is_success());

    // Get the body of the response
    let tracing_body = tracing_res.bytes().await;
    let (tracing_body, mut child) = child
        .with_timeout(LAUNCH_DELAY)
        .kill_on_error(tracing_body)?;

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure the endpoint header is correct
    // The header is split over two lines. But we don't want to require line
    // breaks at a specific word, so we run two checks for different substrings.

    output.any_output_line_contains(
        "HTTP endpoint allows dynamic control of the filter",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    output.any_output_line_contains(
        "tracing events",
        &body,
        "tracing filter endpoint response",
        "the tracing response header",
    )?;
    std::str::from_utf8(&tracing_body)
        .expect("unexpected invalid UTF-8 in tracing filter response");

    // Make sure endpoint requests change the filter
    output.any_output_line_contains(
        "zebrad=debug",
        &tracing_body,
        "tracing filter endpoint response",
        "the modified tracing filter",
    )?;
    std::str::from_utf8(&tracing_body)
        .expect("unexpected invalid UTF-8 in modified tracing filter response");

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;

    Ok(())
}

/// Test that the JSON-RPC endpoint responds to a request,
/// when configured with a single thread.
#[tokio::test]
async fn rpc_endpoint_single_thread() -> Result<()> {
    rpc_endpoint(false).await
}

/// Test that the JSON-RPC endpoint responds to a request,
/// when configured with multiple threads.
#[tokio::test]
async fn rpc_endpoint_parallel_threads() -> Result<()> {
    rpc_endpoint(true).await
}

/// Test that the JSON-RPC endpoint responds to a request.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
#[tracing::instrument]
async fn rpc_endpoint(parallel_cpu_threads: bool) -> Result<()> {
    use serde_json::Value;
    use zebra_node_services::rpc_client::RpcRequestClient;

    let _init_guard = zebra_test::init();
    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = os_assigned_rpc_port_config(parallel_cpu_threads, &Mainnet)?;
    config.network.cache_dir = false.into();

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?.with_timeout(LAUNCH_DELAY);

    // Wait until port is open.
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;
    // Create an http client
    let client = RpcRequestClient::new(rpc_address);

    // Make the call to the `getinfo` RPC method
    let res = client.call("getinfo", "[]".to_string()).await?;

    // Test rpc endpoint response
    assert!(res.status().is_success());

    let body = res.bytes().await;
    let (body, mut child) = child.with_timeout(LAUNCH_DELAY).kill_on_error(body)?;

    let parsed: Value = serde_json::from_slice(&body)?;

    // Check that we have at least 4 characters in the `build` field.
    let build = parsed["result"]["build"].as_str().unwrap();
    assert!(build.len() > 4, "Got {build}");

    // Check that the `subversion` field has "Zebra" in it.
    let subversion = parsed["result"]["subversion"].as_str().unwrap();
    assert!(subversion.contains("Zebra"), "Got {subversion}");

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other zebrad tests running?")?;

    Ok(())
}

/// Test that the JSON-RPC endpoint responds to requests with different content types.
///
/// This test ensures that the curl examples of zcashd rpc methods will also work in Zebra.
///
/// https://zcash.github.io/rpc/getblockchaininfo.html
#[tokio::test]
async fn rpc_endpoint_client_content_type() -> Result<()> {
    use zebra_node_services::rpc_client::RpcRequestClient;

    let _init_guard = zebra_test::init();
    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    let mut config = os_assigned_rpc_port_config(true, &Mainnet)?;
    config.network.cache_dir = false.into();

    let dir = testdir()?.with_config(&mut config)?;
    let mut child = dir.spawn_child(args!["start"])?.with_timeout(LAUNCH_DELAY);

    // Wait until port is open.
    let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

    // Create an http client
    let client = RpcRequestClient::new(rpc_address);

    // Call to `getinfo` RPC method with a no content type.
    let res = client
        .call_with_no_content_type("getinfo", "[]".to_string())
        .await?;

    // Zebra will insert valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain`.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "text/plain".to_string())
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain` content type as the zcashd rpc docs.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "text/plain;".to_string())
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a `text/plain; other string` content type.
    let res = client
        .call_with_content_type(
            "getinfo",
            "[]".to_string(),
            "text/plain; other string".to_string(),
        )
        .await?;

    // Zebra will replace to the valid `application/json` content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with a valid `application/json` content type.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "application/json".to_string())
        .await?;

    // Zebra will not replace valid content type and succeed.
    assert!(res.status().is_success());

    // Call to `getinfo` RPC method with invalid string as content type.
    let res = client
        .call_with_content_type("getinfo", "[]".to_string(), "whatever".to_string())
        .await?;

    // Zebra will not replace unrecognized content type and fail.
    assert!(res.status().is_client_error());

    Ok(())
}

use crate::types::LoadTestResult;
use anyhow::Result;
use hdrhistogram::Histogram;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct LoadConfig {
    pub url: String,
    pub users: u32,
    pub duration_secs: u64,
}

pub async fn run_load_test(config: LoadConfig) -> Result<LoadTestResult> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(config.users as usize + 1)
        // Keep connections alive across requests for accurate throughput
        .connection_verbose(false)
        .build()?;
    let client = Arc::new(client);

    let total_requests = Arc::new(AtomicU64::new(0));
    let failed_requests = Arc::new(AtomicU64::new(0));

    let pb = ProgressBar::new(config.duration_secs);
    pb.set_style(
        ProgressStyle::with_template(
            " {spinner:.yellow} Load test [{bar:40.yellow/white}] {pos}/{len}s  {msg}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );
    pb.enable_steady_tick(Duration::from_millis(100));

    let deadline = Instant::now() + Duration::from_secs(config.duration_secs);

    // Spawn N concurrent user tasks; each owns a local histogram to avoid contention
    let mut handles = vec![];
    for _ in 0..config.users {
        let client = Arc::clone(&client);
        let total = Arc::clone(&total_requests);
        let failed = Arc::clone(&failed_requests);
        let url = config.url.clone();

        handles.push(tokio::spawn(async move {
            let mut local_hist: Histogram<u64> = Histogram::new(3).unwrap();
            while Instant::now() < deadline {
                let t = Instant::now();
                let result = client.get(&url).send().await;

                match result {
                    Ok(resp) => {
                        let ok = resp.status().is_success();
                        // Drain body so the connection is returned to the pool
                        resp.bytes().await.ok();
                        let elapsed_us = t.elapsed().as_micros() as u64;
                        total.fetch_add(1, Ordering::Relaxed);
                        if !ok {
                            failed.fetch_add(1, Ordering::Relaxed);
                        }
                        local_hist.record(elapsed_us).ok();
                    }
                    Err(_) => {
                        let elapsed_us = t.elapsed().as_micros() as u64;
                        total.fetch_add(1, Ordering::Relaxed);
                        failed.fetch_add(1, Ordering::Relaxed);
                        local_hist.record(elapsed_us).ok();
                    }
                }
            }
            local_hist
        }));
    }

    // Update progress bar with live stats every second
    let pb_clone = pb.clone();
    let total_clone = Arc::clone(&total_requests);
    let failed_clone = Arc::clone(&failed_requests);
    let dur = config.duration_secs;
    tokio::spawn(async move {
        let start = Instant::now();
        let mut last_total = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let elapsed = start.elapsed().as_secs();
            if elapsed > dur {
                break;
            }
            let cur_total = total_clone.load(Ordering::Relaxed);
            let cur_failed = failed_clone.load(Ordering::Relaxed);
            let rps = cur_total - last_total;
            last_total = cur_total;
            let err_pct = if cur_total > 0 {
                (cur_failed as f64 / cur_total as f64) * 100.0
            } else {
                0.0
            };
            pb_clone.set_position(elapsed.min(dur));
            pb_clone.set_message(format!(
                "RPS: {rps}  total: {cur_total}  errors: {err_pct:.1}%"
            ));
        }
    });

    // Wait for all workers and merge histograms
    let mut merged: Histogram<u64> = Histogram::new(3)?;
    for h in handles {
        if let Ok(local) = h.await {
            merged.add(local).ok();
        }
    }
    pb.finish_and_clear();

    let total = total_requests.load(Ordering::Relaxed);
    let failed = failed_requests.load(Ordering::Relaxed);
    let successful = total - failed;

    let mean = if merged.len() > 0 { merged.mean() / 1000.0 } else { 0.0 };
    let min = merged.min() as f64 / 1000.0;
    let max = merged.max() as f64 / 1000.0;
    let p50 = merged.value_at_percentile(50.0) as f64 / 1000.0;
    let p90 = merged.value_at_percentile(90.0) as f64 / 1000.0;
    let p99 = merged.value_at_percentile(99.0) as f64 / 1000.0;

    Ok(LoadTestResult {
        url: config.url,
        users: config.users,
        duration_secs: config.duration_secs,
        total_requests: total,
        successful_requests: successful,
        failed_requests: failed,
        error_rate_pct: if total > 0 {
            (failed as f64 / total as f64) * 100.0
        } else {
            0.0
        },
        throughput_rps: total as f64 / config.duration_secs as f64,
        latency_p50_ms: p50,
        latency_p90_ms: p90,
        latency_p99_ms: p99,
        latency_min_ms: min,
        latency_max_ms: max,
        latency_mean_ms: mean,
    })
}

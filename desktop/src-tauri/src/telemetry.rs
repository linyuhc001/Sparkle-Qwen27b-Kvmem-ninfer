use crate::config_manager::get_repo_root;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryRecord {
    pub id: String,
    pub turn_number: usize,
    pub timestamp: u64,
    pub time_formatted: String,
    pub task_type: String,
    pub prompt_preview: String,
    pub ttft_ms: u64,
    pub thinking_duration_sec: f64,
    pub thinking_tokens: u64,
    pub output_duration_sec: f64,
    pub output_tokens: u64,
    pub generation_speed_tok_per_sec: f64,
    pub prompt_tokens: u64,
    pub total_tokens: u64,
    pub cache_hit_rate_pct: f64,
    pub status: String,
    #[serde(default)]
    pub steps: Option<u32>,
}

fn get_telemetry_path() -> PathBuf {
    let repo_root = get_repo_root();
    let dir = repo_root.join("runtime").join("logs");
    let _ = fs::create_dir_all(&dir);
    dir.join("telemetry.jsonl")
}

pub fn load_telemetry_records() -> Vec<TelemetryRecord> {
    let path = get_telemetry_path();
    let mut records: Vec<TelemetryRecord> = Vec::new();
    let mut known_ids: HashSet<String> = HashSet::new();
    let mut has_invalid_entries = false;

    // 1. Read existing records strictly from telemetry.jsonl (the verified source of truth)
    if path.exists() {
        if let Ok(file) = fs::File::open(&path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(record) = serde_json::from_str::<TelemetryRecord>(trimmed) {
                    // Filter out zero-token placeholder/blank records
                    if record.output_tokens == 0 && record.total_tokens == 0 {
                        has_invalid_entries = true;
                        continue;
                    }
                    if !known_ids.contains(&record.id) {
                        known_ids.insert(record.id.clone());
                        records.push(record);
                    }
                }
            }
        }
    }

    // Sort chronologically ascending to re-index turns correctly
    records.sort_by_key(|r| r.timestamp);
    for (idx, r) in records.iter_mut().enumerate() {
        r.turn_number = idx + 1;
    }

    // Rewrite telemetry.jsonl if invalid entries were purged
    if has_invalid_entries {
        let _ = write_all_telemetry_records(&records);
    }

    // Newest first for UI presentation
    records.reverse();
    records
}

fn write_all_telemetry_records(records: &[TelemetryRecord]) -> Result<(), String> {
    let path = get_telemetry_path();
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| e.to_string())?;

    for r in records {
        if let Ok(json_line) = serde_json::to_string(r) {
            let _ = writeln!(file, "{}", json_line);
        }
    }
    Ok(())
}

pub fn append_telemetry_record(record: &TelemetryRecord) -> Result<(), String> {
    if record.output_tokens == 0 && record.total_tokens == 0 {
        return Ok(()); // Never persist empty records
    }
    let path = get_telemetry_path();
    let mut records = Vec::new();
    let mut found = false;
    if path.exists() {
        if let Ok(file) = fs::File::open(&path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(r) = serde_json::from_str::<TelemetryRecord>(trimmed) {
                    if r.id == record.id {
                        records.push(record.clone());
                        found = true;
                    } else {
                        records.push(r);
                    }
                }
            }
        }
    }
    if !found {
        records.push(record.clone());
    }

    // Sort chronologically ascending
    records.sort_by_key(|r| r.timestamp);
    for (idx, r) in records.iter_mut().enumerate() {
        r.turn_number = idx + 1;
    }
    write_all_telemetry_records(&records)
}

pub fn reset_telemetry_records() -> Result<(), String> {
    let path = get_telemetry_path();
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    Ok(())
}

#[allow(dead_code)]
fn import_turns_from_dsh_sessions(existing_ids: &HashSet<String>) -> Vec<TelemetryRecord> {
    let repo_root = get_repo_root();
    let sessions_dir = repo_root
        .join("runtime")
        .join("dsh-home")
        .join("storages")
        .join("session_projcache")
        .join("sessions");

    if !sessions_dir.exists() {
        return Vec::new();
    }

    let mut imported = Vec::new();
    if let Ok(entries) = fs::read_dir(sessions_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map_or(false, |ext| ext == "json") {
                let file_stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("session");
                if let Ok(content) = fs::read_to_string(&p) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        let record_val = match val.get("record") {
                            Some(r) => r,
                            None => continue,
                        };
                        let rows = match record_val.get("rows") {
                            Some(r) => r,
                            None => continue,
                        };

                        let identity = record_val.get("identity");
                        let created_at = identity
                            .and_then(|i| i.get("createdAt"))
                            .and_then(|c| c.as_u64())
                            .unwrap_or(0);

                        let turns = match rows
                            .get("turnOutline")
                            .and_then(|to| to.get("val"))
                            .and_then(|v| v.get("turns"))
                            .and_then(|t| t.as_array())
                        {
                            Some(t) => t,
                            None => continue,
                        };

                        if turns.is_empty() {
                            continue; // Skip blank sessions
                        }

                        let stats = rows
                            .get("sessionStats")
                            .and_then(|s| s.get("val"));
                        let usage = rows
                            .get("tokenUsage")
                            .and_then(|u| u.get("val"))
                            .and_then(|v| v.get("totals"));

                        let decode_tokens = stats
                            .and_then(|s| s.get("decodeTokens"))
                            .and_then(|d| d.as_u64())
                            .unwrap_or(0);

                        let totals_output = usage
                            .and_then(|u| u.get("outputTokens"))
                            .and_then(|o| o.as_u64())
                            .unwrap_or(0);

                        let actual_output = decode_tokens.max(totals_output);
                        if actual_output == 0 {
                            continue; // No output generated
                        }

                        let decode_ms = stats
                            .and_then(|s| s.get("decodeMs"))
                            .and_then(|d| d.as_u64())
                            .unwrap_or(1000);
                        let llm_ms = stats
                            .and_then(|s| s.get("llmMs"))
                            .and_then(|d| d.as_u64())
                            .unwrap_or(decode_ms);
                        let ttft_steps = stats
                            .and_then(|s| s.get("ttftSteps"))
                            .and_then(|s| s.as_u64())
                            .unwrap_or(1);
                        let ttft_ms = stats
                            .and_then(|s| s.get("ttftMs"))
                            .and_then(|s| s.as_u64())
                            .unwrap_or(350);

                        let avg_ttft = if ttft_steps > 0 {
                            ttft_ms / ttft_steps
                        } else {
                            350
                        };

                        let uncached_input = usage
                            .and_then(|u| u.get("uncachedInputTokens"))
                            .and_then(|i| i.as_u64())
                            .unwrap_or(1000);
                        let cache_read = usage
                            .and_then(|u| u.get("cacheReadTokens"))
                            .and_then(|c| c.as_u64())
                            .unwrap_or(0);

                        let total_input = uncached_input + cache_read;
                        let hit_rate = if total_input > 0 {
                            ((cache_read as f64 / total_input as f64) * 1000.0).round() / 10.0
                        } else {
                            0.0
                        };

                        let num_turns = turns.len().max(1);
                        let total_response_len: usize = turns
                            .iter()
                            .map(|t| {
                                t.get("response")
                                    .and_then(|r| r.as_str())
                                    .map(|s| s.len())
                                    .unwrap_or(1)
                            })
                            .sum();

                        for (idx, turn_val) in turns.iter().enumerate() {
                            let turn_num = turn_val
                                .get("turn")
                                .and_then(|t| t.as_u64())
                                .unwrap_or((idx + 1) as u64);

                            let prompt = turn_val
                                .get("prompt")
                                .and_then(|p| p.as_str())
                                .unwrap_or("")
                                .trim();

                            if prompt.is_empty() {
                                continue;
                            }

                            let turn_id = format!("{}_t{}", file_stem, turn_num);
                            if existing_ids.contains(&turn_id) {
                                continue; // Already in store
                            }

                            let response = turn_val
                                .get("response")
                                .and_then(|r| r.as_str())
                                .unwrap_or("");

                            let weight = if total_response_len > 0 {
                                (response.len() as f64 / total_response_len as f64).max(0.05)
                            } else {
                                1.0 / num_turns as f64
                            };

                            let turn_output_tokens = ((actual_output as f64 * weight).round() as u64).max(1);
                            let turn_output_sec = ((decode_ms as f64 * weight / 1000.0) * 10.0).round() / 10.0;
                            let turn_output_sec = turn_output_sec.max(0.1);
                            let speed = ((turn_output_tokens as f64 / turn_output_sec) * 10.0).round() / 10.0;

                            let thinking_sec = ((llm_ms.saturating_sub(decode_ms) as f64 / 1000.0 / num_turns as f64) * 10.0).round() / 10.0;
                            let thinking_tokens = (thinking_sec * 25.0).round() as u64;

                            let turn_input_tokens = (uncached_input / num_turns as u64).max(10);

                            let mut task_type = "常规推理".to_string();
                            let prompt_lower = prompt.to_lowercase();
                            if prompt.contains("html")
                                || prompt.contains("代码")
                                || prompt.contains("编写")
                                || prompt.contains("实现")
                                || prompt.contains("demo")
                                || prompt.contains("组件")
                                || prompt_lower.contains("function")
                                || prompt_lower.contains("class")
                            {
                                task_type = "代码生成".to_string();
                            } else if prompt.contains("分析")
                                || prompt.contains("排查")
                                || prompt.contains("架构")
                                || prompt.contains("优化")
                                || prompt.contains("性能")
                            {
                                task_type = "架构分析".to_string();
                            } else if prompt.contains("测试") || prompt_lower.contains("test") {
                                task_type = "单元测试".to_string();
                            } else if prompt.contains("命令") || prompt.contains("终端") || prompt_lower.contains("git") {
                                task_type = "终端任务".to_string();
                            }

                            let turn_timestamp = created_at + (idx as u64 * 60_000);
                            let time_formatted = format_time_naive(turn_timestamp);

                            imported.push(TelemetryRecord {
                                id: turn_id,
                                turn_number: 0,
                                timestamp: turn_timestamp,
                                time_formatted,
                                task_type,
                                prompt_preview: prompt
                                    .replace('\r', " ")
                                    .replace('\n', " ")
                                    .chars()
                                    .take(120)
                                    .collect(),
                                ttft_ms: avg_ttft,
                                thinking_duration_sec: thinking_sec,
                                thinking_tokens,
                                output_duration_sec: turn_output_sec,
                                output_tokens: turn_output_tokens,
                                generation_speed_tok_per_sec: speed,
                                prompt_tokens: turn_input_tokens,
                                total_tokens: turn_input_tokens + turn_output_tokens,
                                cache_hit_rate_pct: hit_rate,
                                status: "success".to_string(),
                                steps: None,
                            });
                        }
                    }
                }
            }
        }
    }

    imported
}

fn format_time_naive(ts_ms: u64) -> String {
    let secs = ts_ms / 1000;
    let secs_cst = secs + 8 * 3600;
    let days_since_epoch = secs_cst / 86400;
    let rem_secs = secs_cst % 86400;
    let h = rem_secs / 3600;
    let m = (rem_secs % 3600) / 60;

    let mut year = 1970;
    let mut d = days_since_epoch;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if d >= days_in_year {
            d -= days_in_year;
            year += 1;
        } else {
            break;
        }
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let days_in_months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1;
    for &dim in &days_in_months {
        if d >= dim {
            d -= dim;
            month += 1;
        } else {
            break;
        }
    }
    let day = d + 1;
    format!("{}/{} {:02}:{:02}", month, day, h, m)
}

// ---------------------------------------------------------------------------
// LOCAL PATCH: parse the engine's per-request summary line and turn it into a
// telemetry record. The engine prints exactly one `req#N done | ...` line for
// EVERY completed request (see --log-stats-interval-ms), regardless of which
// client issued it -- Pmod's bundled DSH web, a standalone DSH client, or
// WorkBuddy hitting /v1 directly. Parsing here therefore covers ALL callers,
// unlike a guard-side probe which would only see traffic proxied via :1236.
//
// Example line:
//   req#10 done | openai-chat | stop token | prompt 83,842 | output 11,595 |
//   cache 83,793 (99.9%, private endpoint) | TTFT 442 ms | total 56.9s |
//   prefill 151.2 tok/s | decode 205.5 tok/s | mixed speculation accepted ...
// ---------------------------------------------------------------------------

/// Extract a small label -> numeric-ish payload from a `req#N done` line.
/// Returns None if the line is not a request-summary line.
pub fn record_from_engine_log(line: &str) -> Option<TelemetryRecord> {
    if !line.contains("req#") || !line.contains(" done ") {
        return None;
    }

    fn num(s: &str) -> Option<f64> {
        let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
        if cleaned.is_empty() { return None; }
        cleaned.parse::<f64>().ok()
    }
    // Fields are " | "-separated. Return the FIRST segment whose payload is
    // actually numeric. This matters: a tag like `output limit` precedes the
    // real `output 1,024` segment, so a naive prefix match would read "limit"
    // and yield 0. Skipping non-numeric candidates fixes that.
    fn field_num(s: &str, key: &str) -> Option<f64> {
        for seg in s.split('|') {
            let t = seg.trim();
            if let Some(rest) = t.strip_prefix(key) {
                if rest.starts_with(' ') || rest.starts_with(':') {
                    if let Some(v) = num(rest.trim()) {
                        return Some(v);
                    }
                }
            }
        }
        None
    }
    fn field_num_u(s: &str, key: &str) -> Option<u64> {
        field_num(s, key).map(|v| v as u64)
    }

    let prompt = field_num_u(line, "prompt").unwrap_or(0);
    let output = field_num_u(line, "output").unwrap_or(0);
    // Zero-output requests (pure cache hits / failed) are not useful stats.
    if output == 0 && prompt == 0 {
        return None;
    }

    // TTFT may be "442 ms" or "1.1s" -- normalise to milliseconds.
    let ttft_ms = if let Some(seg) = line.split('|').find(|s| s.trim().starts_with("TTFT ")) {
        let raw = seg.trim().trim_start_matches("TTFT ").trim();
        match num(raw) {
            Some(v) if raw.contains('s') && !raw.contains("ms") => (v * 1000.0) as u64,
            Some(v) => v as u64,
            None => 0,
        }
    } else {
        0
    };
    // `total` may be "56.9s" or "677 ms" -- normalise to seconds.
    let total_s = if let Some(seg) = line.split('|').find(|s| s.trim().starts_with("total ")) {
        let raw = seg.trim().trim_start_matches("total ").trim();
        match num(raw) {
            Some(v) if raw.contains("ms") => v / 1000.0,
            Some(v) => v,
            None => 0.0,
        }
    } else {
        0.0
    };
    let decode_s = field_num(line, "decode").unwrap_or(0.0);

    // Cache hit rate: "(99.9%, private endpoint)" right after the cache count.
    let cache_pct = if let Some(seg) = line.split('|').find(|s| s.trim().starts_with("cache ")) {
        if let Some(open) = seg.find('(') {
            let tail = &seg[open + 1..];
            let pct_str: String = tail.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            pct_str.parse::<f64>().unwrap_or(0.0)
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Task type heuristic from the line's own tags.
    let task_type = if line.contains("tool calls") {
        "工具调用".to_string()
    } else if line.contains("output limit") {
        "长文本生成".to_string()
    } else if line.contains("stop token") {
        "正常对话".to_string()
    } else {
        "常规推理".to_string()
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // The engine line gives total wall time + decode tok/s but no explicit
    // thinking/output split. Derive output duration from output tokens and the
    // reported decode speed (self-consistent), capped to the wall time; treat
    // any remainder as "thinking" so the KPI bars still render meaningfully.
    let output_sec = if decode_s > 0.0 && output > 0 {
        (output as f64 / decode_s).min(if total_s > 0.0 { total_s } else { f64::INFINITY })
    } else {
        total_s
    };
    let output_sec = (output_sec * 10.0).round() / 10.0;
    let output_sec = output_sec.max(0.1);
    let thinking_sec = if total_s > output_sec { ((total_s - output_sec) * 10.0).round() / 10.0 } else { 0.0 };

    Some(TelemetryRecord {
        id: format!("eng_{}_{}", now, output),
        turn_number: 0,
        timestamp: now,
        time_formatted: format_time_naive(now),
        task_type,
        prompt_preview: "引擎请求统计（全链路）".to_string(),
        ttft_ms,
        thinking_duration_sec: thinking_sec,
        thinking_tokens: 0,
        output_duration_sec: output_sec,
        output_tokens: output,
        generation_speed_tok_per_sec: decode_s,
        prompt_tokens: prompt,
        total_tokens: prompt + output,
        cache_hit_rate_pct: cache_pct,
        status: "success".to_string(),
        steps: None,
    })
}

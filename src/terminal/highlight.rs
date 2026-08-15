use crate::terminal::RenderCell;
use gpui::Hsla;
use std::collections::HashMap;

trait HslaExt {
    fn into_rgba_like(self, r: u8, g: u8, b: u8) -> Self;
}

impl HslaExt for Hsla {
    fn into_rgba_like(self, r: u8, g: u8, b: u8) -> Self {
        let rf = r as f32 / 255.0;
        let gf = g as f32 / 255.0;
        let bf = b as f32 / 255.0;
        let max = rf.max(gf).max(bf);
        let min = rf.min(gf).min(bf);
        let l = (max + min) / 2.0;
        if max == min {
            return Hsla {
                h: 0.0,
                s: 0.0,
                l,
                a: 1.0,
            };
        }
        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == rf {
            ((gf - bf) / d + if gf < bf { 6.0 } else { 0.0 }) / 6.0
        } else if max == gf {
            ((bf - rf) / d + 2.0) / 6.0
        } else {
            ((rf - gf) / d + 4.0) / 6.0
        };
        Hsla { h, s, l, a: 1.0 }
    }
}

#[derive(Debug, Clone)]
struct HighlightColors {
    // Log levels
    error: Hsla,    // ERROR, ERR
    critical: Hsla, // PANIC, FATAL, EMERGENCY, CRITICAL
    warning: Hsla,  // WARNING, WARN
    info: Hsla,     // INFO, NOTICE
    debug: Hsla,    // DEBUG, TRACE, DBG
    alert: Hsla,    // ALERT

    // Status indicators
    success: Hsla, // SUCCESS, OK, PASS, DONE, COMPLETED
    failure: Hsla, // FAILED, FAIL, FAILURE
    pending: Hsla, // PENDING, WAITING, PROCESSING
    running: Hsla, // RUNNING, ACTIVE, EXECUTING
    stopped: Hsla, // STOPPED, INACTIVE, HALTED, IDLE
    skipped: Hsla, // SKIPPED, SKIP

    // Network
    network_up: Hsla,   // UP, ONLINE, CONNECTED
    network_down: Hsla, // DOWN, OFFLINE, UNREACHABLE
    timeout: Hsla,      // TIMEOUT, TIMED OUT
    refused: Hsla,      // REFUSED, REJECTED, DENIED

    // Security & Auth
    security: Hsla, // SSH, SSL, TLS, CERTIFICATE
    auth: Hsla,     // AUTHENTICATED, AUTHORIZED, LOGIN
    danger: Hsla,   // ROOT, SUDO, PASSWORD, SECRET

    // Operations
    started: Hsla,    // START, BOOT, STARTING
    stopped_op: Hsla, // STOP, SHUTDOWN, STOPPING
    restart: Hsla,    // RESTART, RESTARTING
    deploy: Hsla,     // DEPLOY, DEPLOYED, DEPLOYMENT
    crashed: Hsla,    // CRASH, CRASHED, SIGSEGV

    // Resources
    memory: Hsla, // MEMORY, RAM, SWAP, HEAP
    cpu: Hsla,    // CPU, PROCESSOR, CORE
    disk: Hsla,   // DISK, STORAGE, PARTITION, MOUNT

    // HTTP codes
    http_2xx: Hsla, // 200-299 Success
    http_3xx: Hsla, // 300-399 Redirect
    http_4xx: Hsla, // 400-499 Client Error
    http_5xx: Hsla, // 500-599 Server Error

    // Dev / Exceptions
    exception: Hsla,  // Exception, Traceback, Error type
    deprecated: Hsla, // DEPRECATED, TODO, FIXME

    // Existing
    network: Hsla, // IP addresses
    url: Hsla,     // http://, https://
    port: Hsla,    // :22, :443, etc.
}

fn hsla(r: u8, g: u8, b: u8) -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: 0.0,
        a: 1.0,
    }
    .into_rgba_like(r, g, b)
}

fn highlight_colors() -> HighlightColors {
    HighlightColors {
        // Log levels
        error: hsla(224, 96, 96),     // #E06060 red
        critical: hsla(255, 50, 50),  // #FF3232 bright red
        warning: hsla(232, 201, 122), // #E8C97A yellow
        info: hsla(108, 180, 238),    // #6CB4EE blue
        debug: hsla(130, 140, 155),   // #828C9B gray
        alert: hsla(213, 126, 234),   // #D57EEA bright magenta

        // Status
        success: hsla(126, 198, 153), // #7EC699 green
        failure: hsla(232, 168, 124), // #E8A87C orange
        pending: hsla(232, 201, 122), // #E8C97A yellow
        running: hsla(86, 206, 234),  // #56CEEA cyan
        stopped: hsla(160, 165, 175), // #A0A5AF gray
        skipped: hsla(199, 146, 234), // #C792EA purple

        // Network
        network_up: hsla(126, 198, 153), // #7EC699 green
        network_down: hsla(224, 96, 96), // #E06060 red
        timeout: hsla(213, 126, 234),    // #D57EEA magenta
        refused: hsla(245, 160, 80),     // #F5A050 orange

        // Security
        security: hsla(86, 206, 234), // #56CEEA cyan
        auth: hsla(126, 198, 153),    // #7EC699 green
        danger: hsla(180, 50, 50),    // #B43232 dark red

        // Operations
        started: hsla(126, 198, 153),  // #7EC699 green
        stopped_op: hsla(224, 96, 96), // #E06060 red
        restart: hsla(232, 201, 122),  // #E8C97A yellow
        deploy: hsla(100, 210, 140),   // #64D28C bright green
        crashed: hsla(255, 50, 50),    // #FF3232 bright red

        // Resources
        memory: hsla(199, 146, 234), // #C792EA purple
        cpu: hsla(86, 206, 234),     // #56CEEA cyan
        disk: hsla(108, 180, 238),   // #6CB4EE blue

        // HTTP codes
        http_2xx: hsla(126, 198, 153), // #7EC699 green
        http_3xx: hsla(86, 206, 234),  // #56CEEA cyan
        http_4xx: hsla(232, 201, 122), // #E8C97A yellow
        http_5xx: hsla(224, 96, 96),   // #E06060 red

        // Dev
        exception: hsla(224, 96, 96),   // #E06060 red
        deprecated: hsla(245, 160, 80), // #F5A050 orange

        // Existing
        network: hsla(199, 146, 234), // #C792EA purple
        url: hsla(86, 212, 199),      // #56D4C7 teal
        port: hsla(130, 170, 200),    // #82AAC8 muted teal
    }
}

/// Highlight all occurrences of keyword list in `text`, writing to `map`.
/// Case-insensitive, matches inside larger words (e.g. "my_ERROR" highlights "ERROR").
/// Each keyword only matches once per position (no overlapping highlights).
fn highlight_keywords(
    map: &mut HashMap<(i32, i32), Hsla>,
    text: &str,
    byte_to_col: &[i32],
    row_i32: i32,
    keywords: &[&str],
    color: Hsla,
) {
    for &kw in keywords {
        let kw_lower: Vec<u8> = kw.bytes().map(|b| b.to_ascii_lowercase()).collect();
        let text_bytes = text.as_bytes();
        let text_lower: Vec<u8> = text_bytes.iter().map(|b| b.to_ascii_lowercase()).collect();
        let mut start = 0;
        while start + kw_lower.len() <= text_lower.len() {
            if text_lower[start..].starts_with(&kw_lower) {
                let abs = start;
                let start_col = byte_to_col[abs];
                let end_col = byte_to_col[(abs + kw.len() - 1).min(byte_to_col.len() - 1)];
                for c in start_col..=end_col {
                    map.entry((row_i32, c)).or_insert(color);
                }
                start = abs + kw.len();
            } else {
                start += 1;
            }
        }
    }
}

/// Highlight HTTP status codes (200, 301, 404, 500, etc.)
/// Only matches specific common HTTP codes, not all 3-digit numbers.
fn highlight_http_codes(
    map: &mut HashMap<(i32, i32), Hsla>,
    text: &str,
    byte_to_col: &[i32],
    row_i32: i32,
    colors: &HighlightColors,
) {
    let bytes = text.as_bytes();
    let len = bytes.len();

    // Specific HTTP codes to highlight
    const HTTP_CODES: &[(u16, bool)] = &[
        // 2xx
        (200, true),
        (201, true),
        (202, true),
        (204, true),
        (206, true),
        // 3xx
        (301, true),
        (302, true),
        (304, true),
        (307, true),
        (308, true),
        // 4xx
        (400, true),
        (401, true),
        (403, true),
        (404, true),
        (405, true),
        (408, true),
        (409, true),
        (410, true),
        (422, true),
        (429, true),
        // 5xx
        (500, true),
        (502, true),
        (503, true),
        (504, true),
    ];

    for i in 0..len.saturating_sub(2) {
        if !bytes[i].is_ascii_digit()
            || !bytes[i + 1].is_ascii_digit()
            || !bytes[i + 2].is_ascii_digit()
        {
            continue;
        }

        let code: u16 = ((bytes[i] - b'0') as u16) * 100
            + ((bytes[i + 1] - b'0') as u16) * 10
            + ((bytes[i + 2] - b'0') as u16);

        // Only match specific codes
        if !HTTP_CODES.iter().any(|&(c, _)| c == code) {
            continue;
        }

        // Must be at a boundary (not part of a longer number)
        let before_ok = i == 0 || !bytes[i - 1].is_ascii_digit();
        let after_ok = i + 3 >= len || !bytes[i + 3].is_ascii_digit();
        if !before_ok || !after_ok {
            continue;
        }

        let color = match code {
            200..=299 => colors.http_2xx,
            300..=399 => colors.http_3xx,
            400..=499 => colors.http_4xx,
            500..=599 => colors.http_5xx,
            _ => continue,
        };

        let start_col = byte_to_col[i];
        let end_col = byte_to_col[(i + 2).min(byte_to_col.len() - 1)];
        for c in start_col..=end_col {
            map.entry((row_i32, c)).or_insert(color);
        }
    }
}

pub fn highlight_cells(cells: &[RenderCell], rows: usize) -> HashMap<(i32, i32), Hsla> {
    let colors = highlight_colors();

    let mut row_chars: Vec<Vec<(i32, char)>> = vec![Vec::with_capacity(128); rows];
    for rc in cells {
        if rc.row < 0 || (rc.row as usize) >= rows {
            continue;
        }
        row_chars[rc.row as usize].push((rc.col, rc.cell.c));
    }
    for row in row_chars.iter_mut() {
        row.sort_by_key(|&(col, _)| col);
    }

    let mut map = HashMap::new();

    let mut chars_buf = String::with_capacity(128);
    let mut byte_to_col: Vec<i32> = Vec::with_capacity(128);

    for (row_idx, row) in row_chars.iter().enumerate() {
        if row.is_empty() {
            continue;
        }
        let row_i32 = row_idx as i32;

        chars_buf.clear();
        byte_to_col.clear();

        for &(col, c) in row {
            chars_buf.push(c);
            while byte_to_col.len() < chars_buf.len() {
                byte_to_col.push(col);
            }
        }
        let text = chars_buf.as_str();

        // ── 1. Critical errors (highest priority) ──────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "PANIC",
                "EMERGENCY",
                "FATAL",
                "SEGFAULT",
                "CRITICAL",
                "OOM",
                "OUT OF MEMORY",
                "KERNEL PANIC",
                "CORE DUMPED",
                "BUS ERROR",
            ],
            colors.critical,
        );

        // ── 2. Error keywords ──────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["ERROR", "ERR"],
            colors.error,
        );

        // ── 3. Alert ───────────────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["ALERT"],
            colors.alert,
        );

        // ── 4. Warning keywords ────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["WARNING", "WARN"],
            colors.warning,
        );

        // ── 5. Info keywords ───────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["INFO", "INFORMATION", "NOTICE"],
            colors.info,
        );

        // ── 6. Debug keywords ──────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["DEBUG", "DBG", "TRACE"],
            colors.debug,
        );

        // ── 7. Success status ──────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "SUCCESS",
                "SUCCEEDED",
                "SUCCESSFUL",
                "PASSED",
                "PASS",
                "OK",
                "DONE",
                "COMPLETED",
                "FINISHED",
                "COMPLETE",
            ],
            colors.success,
        );

        // ── 8. Failure status ──────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["FAILED", "FAILURE", "FAIL", "NOT OK"],
            colors.failure,
        );

        // ── 9. Pending / Waiting ───────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["PENDING", "WAITING", "PROCESSING", "IN PROGRESS", "QUEUED"],
            colors.pending,
        );

        // ── 10. Running / Active ───────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["RUNNING", "ACTIVE", "EXECUTING", "IN_PROGRESS", "LIVE"],
            colors.running,
        );

        // ── 11. Stopped / Inactive ─────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["STOPPED", "INACTIVE", "HALTED", "IDLE", "PAUSED"],
            colors.stopped,
        );

        // ── 12. Skipped ────────────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["SKIPPED", "SKIP", "SKIPPING"],
            colors.skipped,
        );

        // ── 13. Network UP ─────────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "UP",
                "ONLINE",
                "CONNECTED",
                "REACHABLE",
                "LISTENING",
                "ESTABLISHED",
                "LINK UP",
            ],
            colors.network_up,
        );

        // ── 14. Network DOWN ───────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "DOWN",
                "OFFLINE",
                "UNREACHABLE",
                "DISCONNECTED",
                "NOT LISTENING",
                "LINK DOWN",
                "NO CARRIER",
            ],
            colors.network_down,
        );

        // ── 15. Timeout ────────────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "TIMEOUT",
                "TIMED OUT",
                "TIMEOUTS",
                "ETIMEDOUT",
                "SLOW",
                "LATENCY",
            ],
            colors.timeout,
        );

        // ── 16. Refused / Denied ───────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "REFUSED",
                "REJECTED",
                "DENIED",
                "PERMISSION DENIED",
                "ACCESS DENIED",
                "FORBIDDEN",
                "BLOCKED",
                "DROP",
            ],
            colors.refused,
        );

        // ── 17. Security / Protocol ────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "SSH",
                "SSHD",
                "SSL",
                "TLS",
                "HTTPS",
                "CERTIFICATE",
                "CERT",
                "FIREWALL",
                "IPTABLES",
                "ACL",
                "WAF",
            ],
            colors.security,
        );

        // ── 18. Authentication ─────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "AUTHENTICATED",
                "ACCEPTED",
                "AUTHORIZED",
                "LOGIN",
                "LOGOUT",
                "LOGGED IN",
                "LOGGED OUT",
                "SESSION",
            ],
            colors.auth,
        );

        // ── 19. Danger (root/sudo/secrets) ─────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "ROOT",
                "SUDO",
                "UID=0",
                "PASSWORD",
                "SECRET",
                "TOKEN",
                "API_KEY",
                "APIKEY",
                "PRIVATE KEY",
                "CREDENTIALS",
            ],
            colors.danger,
        );

        // ── 20. Operations: Start ──────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "STARTED", "START", "STARTING", "BOOT", "BOOTING", "LAUNCHED", "LAUNCH",
            ],
            colors.started,
        );

        // ── 21. Operations: Stop ───────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "STOPPED",
                "STOP",
                "STOPPING",
                "SHUTDOWN",
                "SHUTTING DOWN",
                "TERMINATED",
            ],
            colors.stopped_op,
        );

        // ── 22. Operations: Restart ────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "RESTARTED",
                "RESTART",
                "RESTARTING",
                "RELOAD",
                "RELOADED",
                "RELOADING",
            ],
            colors.restart,
        );

        // ── 23. Operations: Deploy ─────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "DEPLOYED",
                "DEPLOYMENT",
                "DEPLOYING",
                "DEPLOY",
                "ROLLBACK",
                "ROLLED BACK",
                "ROLLING BACK",
                "UPGRADE",
                "UPGRADED",
            ],
            colors.deploy,
        );

        // ── 24. Operations: Crash ──────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "CRASH",
                "CRASHED",
                "CRASHING",
                "SIGSEGV",
                "SIGABRT",
                "SIGKILL",
                "DIED",
                "EXITED",
                "EXIT CODE",
                "CORE DUMP",
            ],
            colors.crashed,
        );

        // ── 25. Resources: Memory ──────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &["MEMORY", "RAM", "HEAP", "STACK", "SWAP", "MEM"],
            colors.memory,
        );

        // ── 26. Resources: CPU ─────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "CPU",
                "PROCESSOR",
                "CORE",
                "CORES",
                "THREAD",
                "THREADS",
                "LOAD",
            ],
            colors.cpu,
        );

        // ── 27. Resources: Disk ────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "DISK",
                "STORAGE",
                "PARTITION",
                "MOUNT",
                "FILESYSTEM",
                "INODE",
                "IOPS",
                "READ",
                "WRITE",
            ],
            colors.disk,
        );

        // ── 28. Dev: Exceptions ────────────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "EXCEPTION",
                "TRACEBACK",
                "THROW",
                "THROWN",
                "STACKTRACE",
                "TYPEERROR",
                "VALUEERROR",
                "KEYERROR",
                "ATTRIBUTEERROR",
                "INDEXERROR",
                "RUNTIMEERROR",
                "IOERROR",
                "OSERROR",
                "NULLPOINTER",
                "NPE",
                "SEGFAULT",
            ],
            colors.exception,
        );

        // ── 29. Dev: Deprecated / TODO ─────────────────────────
        highlight_keywords(
            &mut map,
            text,
            &byte_to_col,
            row_i32,
            &[
                "DEPRECATED",
                "TODO",
                "FIXME",
                "HACK",
                "XXX",
                "WORKAROUND",
                "TEMPORARY",
            ],
            colors.deprecated,
        );

        // ── 30. HTTP status codes ──────────────────────────────
        highlight_http_codes(&mut map, text, &byte_to_col, row_i32, &colors);

        // ── 31. IP addresses ───────────────────────────────────
        for m in find_ip_addresses(text) {
            let ip_len = find_ip_len(&text[m..]);
            let start_col = byte_to_col[m];
            let end_col = byte_to_col[(m + ip_len - 1).min(byte_to_col.len() - 1)];
            for c in start_col..=end_col {
                map.entry((row_i32, c)).or_insert(colors.network);
            }
        }

        // ── 33. Port numbers ───────────────────────────────────
        for m in find_ports(text) {
            let port_len = find_port_len(&text[m..]);
            let start_col = byte_to_col[m];
            let end_col = byte_to_col[(m + port_len - 1).min(byte_to_col.len() - 1)];
            for c in start_col..=end_col {
                map.entry((row_i32, c)).or_insert(colors.port);
            }
        }
    }
    // ── 32. URLs (Logical lines for wrapping support) ───────────
    let logical_lines = build_logical_lines(cells, rows);
    for line in &logical_lines {
        let text = line.text.as_str();
        for m in find_urls(text) {
            let url_len = find_url_len(&text[m..]);
            for i in 0..url_len {
                let idx = m + i;
                if idx < line.byte_to_cell.len() {
                    let (r, c) = line.byte_to_cell[idx];
                    map.entry((r as i32, c as i32)).or_insert(colors.url);
                }
            }
        }
    }

    map
}

fn find_ip_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut dots = 0u8;
    let mut digits = 0u8;
    let mut len = 0usize;

    for &b in bytes {
        match b {
            b'0'..=b'9' => {
                digits += 1;
                if digits > 3 {
                    return 0;
                }
            }
            b'.' => {
                if digits == 0 {
                    return 0;
                }
                dots += 1;
                if dots > 3 {
                    return 0;
                }
                digits = 0;
            }
            _ => break, // Stop at first non-digit/non-dot (including '/')
        }
        len += 1;
    }

    if dots == 3 && digits > 0 { len } else { 0 }
}

fn find_ip_addresses(text: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();

    for i in 0..len {
        if bytes[i].is_ascii_digit() && (i == 0 || is_boundary(bytes[i - 1] as char)) {
            let remaining = &text[i..];
            let ip_len = find_ip_len(remaining);
            if ip_len > 0 {
                let ip_str = &remaining[..ip_len];
                let valid = ip_str.split('.').all(|octet| octet.parse::<u8>().is_ok());
                if valid {
                    positions.push(i);
                }
            }
        }
    }
    positions
}

fn is_boundary(c: char) -> bool {
    !c.is_ascii_alphanumeric() && c != '_'
}

fn find_urls(text: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut start = 0;
    while let Some(pos) = text[start..].find("http") {
        let abs = start + pos;
        let remaining = &text[abs..];
        if (remaining.starts_with("https://") || remaining.starts_with("http://"))
            && (abs == 0 || is_boundary(text.as_bytes()[abs - 1] as char))
        {
            positions.push(abs);
        }
        start = abs + 4;
    }
    positions
}

fn find_url_len(text: &str) -> usize {
    text.find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(text.len())
}

fn find_ports(text: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();

    for i in 0..len {
        if bytes[i] == b':'
            && i + 1 < len
            && bytes[i + 1].is_ascii_digit()
            && (i == 0 || is_boundary(bytes[i - 1] as char) || bytes[i - 1] == b' ')
        {
            let mut j = i + 1;
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let port_str = &text[i + 1..j];
            if let Ok(port) = port_str.parse::<u16>() {
                if port > 0 {
                    let after_ok = j >= len || is_boundary(bytes[j] as char);
                    if after_ok {
                        positions.push(i);
                    }
                }
            }
        }
    }
    positions
}

fn find_port_len(text: &str) -> usize {
    if !text.starts_with(':') {
        return 0;
    }
    let mut len = 1;
    for b in text.as_bytes()[1..].iter() {
        if b.is_ascii_digit() {
            len += 1;
        } else {
            break;
        }
    }
    len
}

#[derive(Clone)]
pub struct LogicalLine<'a> {
    pub text: String,
    pub byte_to_cell: Vec<(usize, usize)>,
    pub row_cells: Vec<&'a RenderCell>,
}

pub fn build_logical_lines<'a>(cells: &'a [RenderCell], rows: usize) -> Vec<LogicalLine<'a>> {
    let mut row_chars: Vec<Vec<&RenderCell>> = vec![Vec::with_capacity(128); rows];
    for rc in cells {
        if rc.row < 0 || (rc.row as usize) >= rows {
            continue;
        }
        row_chars[rc.row as usize].push(rc);
    }
    for row in row_chars.iter_mut() {
        row.sort_by_key(|rc| rc.col);
    }

    let mut logical_lines = Vec::new();
    let mut current_line: Option<LogicalLine> = None;

    for (row_idx, row_cells) in row_chars.into_iter().enumerate() {
        if row_cells.is_empty() {
            if let Some(line) = current_line.take() {
                logical_lines.push(line);
            }
            continue;
        }

        let wraps_from_prev = row_idx > 0 && {
            current_line.as_ref().is_some_and(|line| {
                line.row_cells.last().is_some_and(|rc| {
                    rc.cell
                        .flags
                        .contains(alacritty_terminal::term::cell::Flags::WRAPLINE)
                })
            })
        };

        if !wraps_from_prev {
            if let Some(line) = current_line.take() {
                logical_lines.push(line);
            }
        }

        let mut line = current_line.take().unwrap_or_else(|| LogicalLine {
            text: String::with_capacity(128),
            byte_to_cell: Vec::with_capacity(128),
            row_cells: Vec::new(),
        });

        for rc in row_cells {
            line.text.push(rc.cell.c);
            let end_len = line.text.len();
            while line.byte_to_cell.len() < end_len {
                line.byte_to_cell.push((rc.row as usize, rc.col as usize));
            }
            line.row_cells.push(rc);
        }

        current_line = Some(line);
    }

    if let Some(line) = current_line.take() {
        logical_lines.push(line);
    }

    logical_lines
}

pub fn find_url_at_cell(
    cells: &[RenderCell],
    rows: usize,
    row: usize,
    col: usize,
) -> Option<(String, Vec<(usize, usize)>)> {
    let logical_lines = build_logical_lines(cells, rows);
    for line in logical_lines {
        let text = line.text.as_str();
        for m in find_urls(text) {
            let url_len = find_url_len(&text[m..]);
            let mut url_cells = Vec::with_capacity(url_len);
            let mut matched = false;
            for i in 0..url_len {
                let idx = m + i;
                if idx < line.byte_to_cell.len() {
                    let (r, c) = line.byte_to_cell[idx];
                    if r == row && c == col {
                        matched = true;
                    }
                    url_cells.push((r, c));
                }
            }
            if matched {
                let url_str = text[m..m + url_len].to_string();
                return Some((url_str, url_cells));
            }
        }
    }
    None
}

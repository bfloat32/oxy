//! The `docker:` suite: canned `docker inspect` lines through the same
//! pipeline the provider runs — docker does not exist on every machine
//! the tests do, so the fixtures are the verification. The canned answer
//! is the same four containers the `docker` fakebin stub prints for
//! `oxy test --cases`, so the two layers are exercised on one story.

use serde_json::{Map, Value, json};

use super::rows;

/// `docker ps` order in the fixture: a restarting container first, then
/// the two running ones apart, the exited one between — so the band sort
/// and the stable order inside a band both have something to prove.
const INSPECT_LINES: &str = concat!(
    "{\"id\":\"cccc3333dddd4444eeee5555ffff6666aaaa77778888bbbb\",",
    "\"name\":\"/crashy\",\"image\":\"broken:latest\",\"restarts\":7,",
    "\"state\":{\"Status\":\"restarting\",\"ExitCode\":1,\"OOMKilled\":false,",
    "\"StartedAt\":\"2025-05-30T12:00:00Z\",\"FinishedAt\":\"2025-06-01T00:00:00Z\"},",
    "\"ports\":{},\"memCap\":0,\"nanoCpus\":0,\"project\":\"\",\"service\":\"\"}\n",
    "{\"id\":\"aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666\",",
    "\"name\":\"/web\",\"image\":\"nginx:1.27\",\"restarts\":0,",
    "\"state\":{\"Status\":\"running\",\"Running\":true,\"ExitCode\":0,\"OOMKilled\":false,",
    "\"StartedAt\":\"2025-01-01T00:00:00Z\",\"Health\":{\"Status\":\"healthy\"}},",
    "\"ports\":{\"8080/tcp\":[{\"HostIp\":\"0.0.0.0\",\"HostPort\":\"8080\"},",
    "{\"HostIp\":\"::\",\"HostPort\":\"8080\"}],",
    "\"8443/tcp\":[{\"HostIp\":\"127.0.0.1\",\"HostPort\":\"8443\"}],",
    "\"9090/tcp\":[{\"HostIp\":\"::1\",\"HostPort\":\"9090\"}],\"6379/tcp\":null},",
    "\"memCap\":536870912,\"nanoCpus\":150000000,",
    "\"project\":\"shop\",\"service\":\"storefront\"}\n",
    "{\"id\":\"dddd4444eeee5555ffff6666aaaa77778888bbbb9999cccc\",",
    "\"name\":\"/old\",\"image\":\"alpine:3.20\",\"restarts\":0,",
    "\"state\":{\"Status\":\"exited\",\"ExitCode\":0,\"OOMKilled\":false,",
    "\"StartedAt\":\"2025-05-01T00:00:00Z\",\"FinishedAt\":\"2025-06-01T12:00:00Z\"},",
    "\"ports\":{\"6379/tcp\":null},\"memCap\":0,\"nanoCpus\":0,",
    "\"project\":\"\",\"service\":\"\"}\n",
    "{\"id\":\"bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa7777\",",
    "\"name\":\"/db\",\"image\":\"postgres:17\",\"restarts\":0,",
    "\"state\":{\"Status\":\"running\",\"Running\":true,\"ExitCode\":0,\"OOMKilled\":false,",
    "\"StartedAt\":\"2025-01-01T00:00:00Z\"},",
    "\"ports\":{\"5432/tcp\":[{\"HostIp\":\"0.0.0.0\",\"HostPort\":\"5432\"}]},",
    "\"memCap\":0,\"nanoCpus\":0,\"project\":\"shop\",\"service\":\"postgres\"}\n",
);

fn items() -> Vec<Value> {
    INSPECT_LINES
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const NOW: i64 = 1_780_000_000;
const CORES: f64 = 16.0;
const HOSTMEM: f64 = 66_035_122_176.0; // 61.5 GiB

fn build(needle: &str) -> Vec<Value> {
    // mod.rs folds the query before it ever reaches rows::build, exactly
    // like the script's `needle="${query,,}"` — tests feed it the same.
    rows::build(
        &items(),
        &Map::new(),
        NOW,
        CORES,
        HOSTMEM,
        &needle.to_lowercase(),
    )
    .unwrap()
}

#[test]
fn the_sort_is_running_then_restarting_then_stopped() {
    let rows = build("");
    assert_eq!(rows.len(), 4);
    let titles: Vec<&str> = rows.iter().map(|r| r["title"].as_str().unwrap()).collect();
    // ps gave crashy, web, old, db; bands reorder to web, db, crashy, old
    // — and inside band 0 the ps order survives.
    assert_eq!(titles, ["web", "db", "crashy", "old"]);
    let bands: Vec<i64> = rows.iter().map(|r| r["band"].as_i64().unwrap()).collect();
    assert_eq!(bands, [0, 0, 1, 2]);
    let scores: Vec<i64> = rows.iter().map(|r| r["score"].as_i64().unwrap()).collect();
    assert_eq!(scores, [90000, 89900, 89800, 89700]);
}

#[test]
fn the_row_shape_is_the_scripts() {
    let rows = build("");
    let web = &rows[0];
    assert_eq!(web["id"], json!("docker:aaaa1111bbbb"));
    assert_eq!(web["view"], json!("docker"));
    assert_eq!(web["title"], json!("web"));
    assert_eq!(web["subtitle"], json!("running"));
    assert_eq!(web["detail"], json!("nginx:1.27"));
    assert_eq!(web["accessory"], json!("8080 8443 9090"));
    assert_eq!(web["cid"], json!("aaaa1111bbbb"));
    assert_eq!(
        web["full"],
        json!("aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666")
    );
    assert_eq!(web["health"], json!("healthy"));
    assert_eq!(web["oom"], json!(false));
    assert_eq!(web["project"], json!("shop"));
    assert_eq!(web["service"], json!("storefront"));
    assert_eq!(web["hostCores"], json!(16));
    assert_eq!(web["hostMem"], json!(HOSTMEM as i64));
    // No sampler has answered yet: cpu/mem are the script's nulls.
    assert_eq!(web["cpu"], Value::Null);
    assert_eq!(web["mem"], Value::Null);
    assert_eq!(web["memBytes"], json!(0));
    assert_eq!(web["memPct"], Value::Null);
}

#[test]
fn ports_dedupe_bracket_ipv6_and_deny_the_db_ports() {
    let rows = build("");
    let ports = rows[0]["ports"].as_array().unwrap();
    // 8080 bound to both 0.0.0.0 and :: collapses to one service; the
    // never-bound 6379 is gone entirely.
    assert_eq!(ports.len(), 3);
    assert_eq!(ports[0]["host"], json!(8080));
    assert_eq!(ports[0]["container"], json!(8080));
    assert_eq!(ports[0]["addr"], json!("localhost"));
    assert_eq!(ports[0]["label"], json!("8080"));
    assert_eq!(ports[0]["web"], json!(true));
    assert_eq!(ports[0]["url"], json!("http://localhost:8080"));
    assert_eq!(
        ports[0]["exec"],
        json!("omarchy-launch-browser 'http://localhost:8080'")
    );
    assert_eq!(ports[1]["url"], json!("http://127.0.0.1:8443"));
    // An IPv6 literal is bracketed only in the URL.
    assert_eq!(ports[2]["addr"], json!("::1"));
    assert_eq!(ports[2]["url"], json!("http://[::1]:9090"));

    // 5432 is on the deny list: copied, not opened.
    let db = rows[1]["ports"].as_array().unwrap();
    assert_eq!(db.len(), 1);
    assert_eq!(db[0]["web"], json!(false));
    assert_eq!(db[0]["url"], json!("localhost:5432"));
    assert_eq!(db[0]["exec"], json!("printf %s 'localhost:5432' | wl-copy"));
}

#[test]
fn the_actions_are_the_scripts_verbatim() {
    let rows = build("");
    let web = &rows[0];
    assert_eq!(
        web["exec"],
        json!(
            "omarchy-launch-tui --app-id=org.omarchy.docker docker logs -f --tail 200 'aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666'"
        )
    );
    let actions = web["actions"].as_array().unwrap();
    let titles: Vec<&str> = actions
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Follow Logs",
            "Shell Inside",
            "Open http://localhost:8080",
            "Open http://127.0.0.1:8443",
            "Open http://[::1]:9090",
            "Stop",
            "Restart",
            "Copy Container ID",
            "Copy Name"
        ]
    );
    assert_eq!(actions[0]["shortcut"], json!("↵"));
    // TERM travels, the name prints first, a failure stays on screen.
    let shell = actions[1]["exec"].as_str().unwrap();
    assert!(shell.starts_with("omarchy-launch-tui --app-id=org.omarchy.docker bash -c 'printf "));
    assert!(shell.contains("docker exec -it -e TERM=xterm-256color"));
    assert!(shell.contains("read -rsn1; }'"));
    // The verb wraps the failure in a notification rather than losing it.
    let stop = actions[5]["exec"].as_str().unwrap();
    assert_eq!(
        stop,
        "out=$(docker stop 'aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666' 2>&1) \
         && omarchy-notification-send 'web stopped' \
         || omarchy-notification-send -u normal 'docker stop failed' \"$out\""
    );

    // A running container gets Stop; a stopped one gets Start; a crash
    // loop gets Stop — it is already trying to run.
    let crashy = &rows[2];
    let titles: Vec<&str> = crashy["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Follow Logs",
            "Stop",
            "Restart",
            "Copy Container ID",
            "Copy Name"
        ]
    );
    let old = &rows[3];
    let titles: Vec<&str> = old["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Follow Logs",
            "Start",
            "Restart",
            "Copy Container ID",
            "Copy Name"
        ]
    );
    assert!(old["exec"].as_str().unwrap().contains("logs -f"));
}

#[test]
fn the_needle_matches_names_images_services_projects_and_ports() {
    let by_title = |rows: &[Value]| -> Vec<String> {
        rows.iter()
            .map(|r| r["title"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(by_title(&build("db")), ["db"]);
    // The compose service is in the haystack.
    assert_eq!(by_title(&build("storefront")), ["web"]);
    // And so are the published ports — `docker:8080` is a real question.
    assert_eq!(by_title(&build("8080")), ["web"]);
    assert_eq!(by_title(&build("5432")), ["db"]);
    // The project groups its containers.
    assert_eq!(by_title(&build("shop")), ["web", "db"]);
    // Nothing matching is an empty answer, not the whole list.
    assert!(build("zzz-no-such").is_empty());
    // Case is folded on both sides.
    assert_eq!(by_title(&build("NGINX")), ["web"]);
}

#[test]
fn the_meters_are_drawn_against_their_denominators() {
    let rows = build("");
    // A 512MiB cap is named as a limit; a quota of 0.15 cores as 15.
    assert_eq!(rows[0]["memCap"], json!(536870912));
    assert_eq!(rows[0]["memCapText"], json!("512MiB limit"));
    assert_eq!(rows[0]["cpuFull"], json!(15));
    assert_eq!(rows[0]["cpuFullText"], json!("0.15 cpus"));
    // No cap means the host is the denominator — and says so.
    assert_eq!(rows[1]["memCap"], json!(HOSTMEM as i64));
    assert_eq!(rows[1]["memCapText"], json!("61.5GiB host"));
    assert_eq!(rows[1]["cpuFull"], json!(1600));
    assert_eq!(rows[1]["cpuFullText"], json!("16 cpus"));
}

#[test]
fn stats_fill_the_live_numbers_when_the_sampler_has_answered() {
    let mut stats = Map::new();
    stats.insert(
        "aaaa1111bbbb".to_string(),
        json!({"id": "aaaa1111bbbb", "cpu": "2.50%", "mem": "12.5MiB / 512MiB", "memPct": "2.44%"}),
    );
    let rows = rows::build(&items(), &stats, NOW, CORES, HOSTMEM, "").unwrap();
    assert_eq!(rows[0]["cpu"], json!(2.5));
    assert_eq!(rows[0]["mem"], json!("12.5MiB"));
    assert_eq!(rows[0]["memBytes"], json!(13107200));
    assert_eq!(rows[0]["memPct"], json!(2.44));
    // A stopped container with a stale stats line still shows them —
    // the file's freshness gate is the caller's, not the row's.
    let mut stats = Map::new();
    stats.insert(
        "dddd4444eeee".to_string(),
        json!({"id": "dddd4444eeee", "cpu": "0.00%", "mem": "0B / 0B", "memPct": "0.00%"}),
    );
    let rows = rows::build(&items(), &stats, NOW, CORES, HOSTMEM, "").unwrap();
    assert_eq!(rows[3]["cpu"], json!(0));
    assert_eq!(rows[3]["memBytes"], json!(0));
}

#[test]
fn since_is_seconds_against_now() {
    let rows = build("");
    // Running since 2025-01-01: an uptime, now minus the start.
    assert_eq!(rows[0]["since"], json!(NOW - 1_735_689_600));
    // Exited at 2025-06-01T12:00:00Z: how long ago it died.
    assert_eq!(rows[3]["since"], json!(NOW - 1_748_779_200));
    // A year-1 stamp — never stopped — clamps to 0 rather than negative.
    let mut items = items();
    items[0]["state"]["FinishedAt"] = json!("0001-01-01T00:00:00Z");
    let rows = rows::build(&items, &Map::new(), NOW, CORES, HOSTMEM, "").unwrap();
    assert_eq!(rows[2]["since"], json!(0));
}

#[test]
fn poison_in_one_container_empties_the_answer() {
    // A non-string name is `sub` on a scalar — the error that killed jq.
    let mut items1 = items();
    items1[0]["name"] = json!(5);
    assert!(rows::build(&items1, &Map::new(), NOW, CORES, HOSTMEM, "").is_none());
    // Same for a scalar `ports` — `to_entries` dies.
    let mut items2 = items();
    items2[0]["ports"] = json!("80/tcp");
    assert!(rows::build(&items2, &Map::new(), NOW, CORES, HOSTMEM, "").is_none());
    // But a narrowed query is the only one that can die on the haystack:
    // a numeric image joins nothing, and an empty needle never asks.
    let mut items3 = items();
    items3[0]["image"] = json!(5);
    assert_eq!(
        rows::build(&items3, &Map::new(), NOW, CORES, HOSTMEM, "").map(|r| r.len()),
        Some(4)
    );
    assert!(rows::build(&items3, &Map::new(), NOW, CORES, HOSTMEM, "x").is_none());
}

#[test]
fn stats_map_is_the_tsv_indexed_by_short_id() {
    let m = rows::stats_map("aaaa1111bbbb\t0.50%\t12.5MiB / 512MiB\t2.44%\nshort\tline\n");
    assert_eq!(m.len(), 1);
    assert_eq!(m["aaaa1111bbbb"]["cpu"], json!("0.50%"));
    assert_eq!(m["aaaa1111bbbb"]["memPct"], json!("2.44%"));
    // Last line wins, like jq's INDEX.
    let m = rows::stats_map("a\t1\tb\tc\na\t2\td\te\n");
    assert_eq!(m["a"]["cpu"], json!("2"));
}

#[test]
fn bytes_and_human_are_the_unit_math() {
    // Private helpers exercised through the two lines they serve: memBytes
    // is bytes() and memCapText is human() — but the pair deserves a look
    // at the edges the containers never hit.
    let stats = |mem: &str| {
        let mut m = Map::new();
        m.insert(
            "aaaa1111bbbb".to_string(),
            json!({"id": "aaaa1111bbbb", "cpu": "0%", "mem": mem, "memPct": "0%"}),
        );
        m
    };
    let rows = rows::build(&items(), &stats("1KiB / 1GiB"), NOW, CORES, HOSTMEM, "").unwrap();
    assert_eq!(rows[0]["memBytes"], json!(1024));
    let rows = rows::build(&items(), &stats("100MB / 1GB"), NOW, CORES, HOSTMEM, "").unwrap();
    assert_eq!(rows[0]["memBytes"], json!(100_000_000));
}

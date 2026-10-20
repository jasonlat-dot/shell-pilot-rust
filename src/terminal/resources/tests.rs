use super::*;

fn sample() -> Sample {
    parse_sample(include_str!("../../../tests/fixtures/resources.txt")).unwrap()
}

#[test]
fn parses_real_proc_units_available_memory_routes_and_disks() {
    let sample = sample();
    assert_eq!(sample.cpu, Some((1000, 800))); // guest ticks must not be counted twice
    assert_eq!(sample.memory_total, Some(1_024_000));
    assert_eq!(sample.memory_used, Some(614_400));
    assert_eq!(sample.boot, Some(1_700_000_000));
    assert_eq!(sample.uptime, Some(1000.0));
    assert_eq!(sample.primary.as_deref(), Some("eth0"));
    assert!(!sample.network.contains_key("lo"));
    assert_eq!(sample.disks.len(), 2);
    assert_eq!(sample.disks[0].mount_point, "/");
    assert_eq!(sample.disks[1].mount_point, "/mnt/data volume");
    assert_eq!(sample.disks[0].total_bytes, 102_400_000);
    assert_eq!(sample.disks[0].use_percent, 40.0);
    let crlf = include_str!("../../../tests/fixtures/resources.txt")
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    assert_eq!(parse_sample(&crlf).unwrap().cpu, sample.cpu);
}
#[test]
fn blank_section_separators_keep_uptime_and_allow_rate_calculation() {
    let text = include_str!("../../../tests/fixtures/resources.txt");
    let old = parse_sample(&text.replace("\r\n", "\n").replace("\n[", "\n \t\n[")).unwrap();
    let next = parse_sample(
        &text
            .replace("1000.00 5000.00", "1005.00 5020.00")
            .replace(
                "cpu 100 0 50 750 50 10 10 30",
                "cpu 150 0 50 900 50 10 10 30",
            )
            .replace("eth0: 1000", "eth0: 6000")
            .replace("0 2000 0", "0 12000 0"),
    )
    .unwrap();
    let result = next.snapshot(Some(&old));
    assert_eq!(result.uptime_seconds, Some(1005.0));
    assert_eq!(result.cpu_percent, Some(25.0));
    assert_eq!(result.network[0].receive_bytes_per_second, Some(1000.0));
    assert_eq!(result.network[0].transmit_bytes_per_second, Some(2000.0));
}
#[test]
fn rates_are_remote_interval_deltas_and_first_sample_is_unknown() {
    let old = sample();
    let first = old.snapshot(None);
    assert_eq!(first.cpu_percent, None);
    assert_eq!(first.network[0].receive_bytes_per_second, None);
    let mut next = sample();
    next.cpu = Some((1200, 950));
    next.uptime = Some(1005.0);
    next.network.insert("eth0".into(), (6000, 12000));
    let result = next.snapshot(Some(&old));
    assert_eq!(result.cpu_percent, Some(25.0));
    let network = result
        .network
        .iter()
        .find(|nic| nic.name == "eth0")
        .unwrap();
    assert_eq!(network.receive_bytes_per_second, Some(1000.0));
    assert_eq!(network.transmit_bytes_per_second, Some(2000.0));
}
#[test]
fn reboot_long_pause_and_counter_reset_do_not_fabricate_spikes() {
    let old = sample();
    for (uptime, boot) in [(5.0, old.boot), (1100.0, old.boot), (1005.0, Some(1))] {
        let mut next = sample();
        next.uptime = Some(uptime);
        next.boot = boot;
        assert_eq!(next.snapshot(Some(&old)).cpu_percent, None);
        assert!(next
            .snapshot(Some(&old))
            .network
            .iter()
            .all(|nic| nic.receive_bytes_per_second.is_none()));
    }
    let mut next = sample();
    next.uptime = Some(1005.0);
    next.cpu = Some((5, 2));
    next.network.insert("eth0".into(), (1, 1));
    let result = next.snapshot(Some(&old));
    assert_eq!(result.cpu_percent, None);
    assert!(result
        .network
        .iter()
        .find(|nic| nic.name == "eth0")
        .unwrap()
        .receive_bytes_per_second
        .is_none());
}
#[test]
fn missing_memavailable_uses_reclaimable_memory_not_just_free() {
    let text = include_str!("../../../tests/fixtures/resources.txt")
        .replace("\r\n", "\n")
        .replace("MemAvailable: 400 kB\n", "");
    let sample = parse_sample(&text).unwrap();
    assert_eq!(
        sample.memory_used,
        Some((1000 - (100 + 50 + 150 + 25 - 5)) * 1024)
    );
}
#[test]
fn missing_fields_remain_unknown_and_invalid_wire_data_is_rejected() {
    let sample = parse_sample("SHELLPILOT_RESOURCES_V1\n[uptime]\n5.0 1.0\n[end]\n").unwrap();
    let result = sample.snapshot(None);
    assert!(result.memory_used_bytes.is_none());
    assert!(result.disks.is_empty());
    for invalid in [
        "",
        "SHELLPILOT_RESOURCES_V1\n[stat]\ncpu 1 2 3 4",
        "SHELLPILOT_RESOURCES_V1\n[uptime]\nNaN\n[end]\n",
    ] {
        assert!(parse_sample(invalid).is_err());
    }
    let unsupported = parse_sample("SHELLPILOT_RESOURCES_V1\nUNSUPPORTED\n")
        .unwrap()
        .snapshot(None);
    assert!(!unsupported.supported);
    assert!(unsupported.cpu_percent.is_none());
}
#[test]
fn malformed_disk_capacity_and_virtual_mounts_are_not_displayed_as_zero() {
    for line in [
        "tmpfs 100 0 100 0% /run",
        "/dev/x 0 0 0 0% /",
        "/dev/x 100 50 50 NaN% /",
        "/dev/x 100 50 50 500% /",
        "/dev/x 18446744073709551615 2 3 50% /",
        "/dev/x 100 50 50 50% /sys/fs/cgroup",
    ] {
        assert!(parse_disk(&line.split_whitespace().collect::<Vec<_>>()).is_none());
    }
}

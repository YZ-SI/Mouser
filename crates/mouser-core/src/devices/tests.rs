//! Ports of tests/test_logi_devices.py, tests/test_device_layouts.py, tests/test_hid_inventory.py
//! and tests/test_hid_diagnostics.py, plus tests for triage #156, #142, #214.

use super::*;

fn ctl(cid: u16, flags: u32) -> ReprogControlDetail {
    ReprogControlDetail::with_flags(cid, flags)
}
fn bare(cid: u16) -> ReprogControlDetail {
    ReprogControlDetail::from_cid(cid)
}
fn key(pid: u16) -> &'static str {
    resolve_device(Some(pid), None).expect("resolves").key
}
fn by_name(name: &str) -> &'static LogiDeviceSpec {
    resolve_device(None, Some(name)).expect("resolves by name")
}
fn has(b: &[&str], k: &str) -> bool {
    b.contains(&k)
}
fn layout_keys(l: &DeviceLayout) -> Vec<&'static str> {
    l.hotspots.iter().map(|h| h.button_key).collect()
}

// ---- LogiDeviceRegistryTests ----

#[test]
fn test_resolve_mx_master_4_by_product_id() {
    // py: test_resolve_mx_master_4_by_product_id
    let d = resolve_device(Some(0xB042), None).unwrap();
    assert_eq!((d.key, d.ui_layout), ("mx_master_4", "mx_master_4"));
}

#[test]
fn test_resolve_mx_master_4_by_hid_product_string() {
    // py: test_resolve_mx_master_4_by_hid_product_string
    assert_eq!(by_name("MX_Master_4").key, "mx_master_4");
}

#[test]
fn test_resolve_mx_master_4_business_pid_to_same_layout() {
    // py: test_resolve_mx_master_4_business_pid_to_same_layout
    let d = resolve_device(Some(0xB048), None).unwrap();
    assert_eq!((d.key, d.ui_layout), ("mx_master_4", "mx_master_4"));
}

#[test]
fn test_resolve_device_by_product_id() {
    // py: test_resolve_device_by_product_id
    let d = resolve_device(Some(0xB034), None).unwrap();
    assert_eq!((d.key, d.display_name), ("mx_master_3s", "MX Master 3S"));
}

#[test]
fn test_resolve_mx_master_3s_business_pid() {
    // py: test_resolve_mx_master_3s_business_pid
    assert_eq!(key(0xB043), "mx_master_3s");
}

#[test]
fn test_resolve_mx_anywhere_3s_uses_layout_key() {
    // py: test_resolve_mx_anywhere_3s_uses_layout_key
    let d = resolve_device(Some(0xB037), None).unwrap();
    assert_eq!(d.key, "mx_anywhere_3s");
    assert_eq!(d.ui_layout, "mx_anywhere_3s");
    assert_eq!(d.image_asset, "logitech-mice/mx_anywhere_3s/mouse.png");
}

#[test]
fn test_resolve_device_by_alias() {
    // py: test_resolve_device_by_alias
    let d = by_name("MX Master 3 for Mac");
    assert_eq!(d.key, "mx_master_3");
    assert!(d.product_ids.contains(&0xB023));
}

#[test]
fn test_resolve_mx_master_3_business_pid() {
    // py: test_resolve_mx_master_3_business_pid
    assert_eq!(key(0xB028), "mx_master_3");
}

#[test]
fn test_resolve_mx_anywhere_3_promoted_pids() {
    // py: test_resolve_mx_anywhere_3_promoted_pids
    for pid in [0xB025, 0xB02D] {
        let d = resolve_device(Some(pid), None).unwrap();
        assert_eq!((d.key, d.ui_layout), ("mx_anywhere_3", "mx_anywhere_3"));
        assert_eq!(d.image_asset, "logitech-mice/mx_anywhere_3/mouse.png");
    }
}

#[test]
fn test_resolve_m650_by_bluetooth_pid() {
    // py: test_resolve_m650_by_bluetooth_pid
    let d = resolve_device(Some(0xB02A), None).unwrap();
    assert_eq!((d.key, d.ui_layout), ("m650", "m650"));
}

#[test]
fn test_resolve_m650_by_common_name_variants() {
    // py: test_resolve_m650_by_common_name_variants
    for n in ["Signature M650", "Logi M650", "Logitech Signature M650"] {
        let d = by_name(n);
        assert_eq!((d.key, d.ui_layout), ("m650", "m650"), "{n}");
    }
}

#[test]
fn test_build_m650_connected_device_info_uses_catalog_layout_and_buttons() {
    // py: test_build_m650_connected_device_info_uses_catalog_layout_and_buttons
    let controls = [bare(0x52), bare(0x53), bare(0x56), bare(0xD7)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB02A),
        product_name: Some("Signature M650"),
        reprog_controls: &controls,
        gesture_cids: Some(&[0xD7]),
        ..Default::default()
    });
    assert_eq!(info.key, "m650");
    assert_eq!(info.display_name, "M650 Signature");
    assert_eq!(info.ui_layout, "m650");
    assert_eq!(info.image_asset, "icons/mouse-simple.svg");
    assert_eq!(info.supported_buttons, ["middle", "xbutton1", "xbutton2"]);
    assert_eq!((info.dpi_min, info.dpi_max), (200, 4000));
}

#[test]
fn test_resolve_m585_m590_by_name_variants() {
    // py: test_resolve_m585_m590_by_name_variants
    for n in ["M585/M590 Multi-Device Mouse", "M590", "M585", "Logitech M590"] {
        let d = by_name(n);
        assert_eq!((d.key, d.ui_layout), ("m585_m590", "m585_m590"), "{n}");
    }
}

#[test]
fn test_shared_receiver_pid_does_not_resolve_to_m585_m590() {
    // py: test_shared_receiver_pid_does_not_resolve_to_m585_m590 (stronger: resolves to nothing)
    assert!(resolve_device(Some(0xC52B), None).is_none());
}

#[test]
fn test_build_m585_m590_connected_device_info_exposes_wheel_tilt() {
    // py: test_build_m585_m590_connected_device_info_exposes_wheel_tilt
    let controls: Vec<_> = [0x50, 0x51, 0x52, 0x53, 0x56, 0x5B, 0x5D, 0xD7].map(bare).to_vec();
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_name: Some("M585/M590 Multi-Device Mouse"),
        reprog_controls: &controls,
        ..Default::default()
    });
    assert_eq!((&*info.key, info.ui_layout), ("m585_m590", "m585_m590"));
    assert_eq!(
        info.supported_buttons,
        ["middle", "xbutton1", "xbutton2", "hscroll_left", "hscroll_right"]
    );
}

#[test]
fn test_mx_anywhere_3s_uses_exact_catalog_layout() {
    // py: test_mx_anywhere_3s_uses_exact_catalog_layout
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB037),
        ..Default::default()
    });
    assert_eq!(info.display_name, "MX Anywhere 3S");
    assert_eq!(info.ui_layout, "mx_anywhere_3s");
    assert_eq!(info.image_asset, "logitech-mice/mx_anywhere_3s/mouse.png");
}

#[test]
fn test_mx_anywhere_2_retained_pids_use_shared_layout_and_metadata() {
    // py: test_mx_anywhere_2_retained_pids_use_shared_layout_and_metadata
    for pid in [0xB013, 0xB01F] {
        let info = build_connected_device_info(&ConnectedDeviceParams {
            product_id: Some(pid),
            ..Default::default()
        });
        let layout = get_device_layout(Some(info.ui_layout));
        assert_eq!(info.key, "mx_anywhere_2");
        assert_eq!(info.display_name, "MX Anywhere 2");
        assert_eq!(info.ui_layout, "mx_anywhere_2s");
        assert_eq!(info.image_asset, "logitech-mice/mx_anywhere_2s/mouse.png");
        assert_eq!(layout.key, "mx_anywhere_2s");
        assert!(layout.interactive);
        assert_eq!(info.supported_buttons, MX_ANYWHERE_EXACT_BUTTONS);
        assert_eq!((info.dpi_min, info.dpi_max), (400, 1600));
        assert_eq!(clamp_dpi(200, Some(&info)), 400);
        assert_eq!(clamp_dpi(1601, Some(&info)), 1600);
    }
}

#[test]
fn test_mx_anywhere_2_receiver_name_resolves_with_shared_receiver_pid() {
    // py: test_mx_anywhere_2_receiver_name_resolves_with_shared_receiver_pid
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        product_name: Some("Wireless Mouse MX Anywhere 2"),
        transport: Some("USB receiver"),
        ..Default::default()
    });
    assert_eq!(info.key, "mx_anywhere_2");
    assert_eq!(info.ui_layout, "mx_anywhere_2s");
    assert_eq!(info.product_id, Some(0xC52B));
    assert_eq!(info.product_name.as_deref(), Some("Wireless Mouse MX Anywhere 2"));
}

#[test]
fn test_exact_mx_anywhere_button_sets_include_expected_controls() {
    // py: test_exact_mx_anywhere_button_sets_include_expected_controls
    // Changed for #142: MX Anywhere 3/3S have no gesture group; 2S keeps it.
    let a2s = get_buttons_for_layout("mx_anywhere_2s").unwrap();
    let a3 = get_buttons_for_layout("mx_anywhere_3").unwrap();
    let a3s = get_buttons_for_layout("mx_anywhere_3s").unwrap();
    for b in [a2s, a3, a3s] {
        assert!(has(b, "hscroll_left") && has(b, "hscroll_right"));
    }
    assert!(has(a2s, "gesture_left") && has(a2s, "gesture_right"));
    assert!(!has(a2s, "mode_shift"));
    assert!(has(a3, "mode_shift") && has(a3s, "mode_shift"));
}

#[test]
fn test_known_product_ids_are_unique() {
    // py: test_known_product_ids_are_unique (also WPIDs)
    let mut seen = std::collections::HashMap::new();
    for d in iter_known_devices() {
        for id in d.product_ids.iter().chain(d.wpids) {
            assert!(seen.insert(*id, d.key).is_none_or(|k| k == d.key), "duplicate id {id:#06X}");
        }
    }
}

#[test]
fn test_all_known_product_ids_resolve_to_their_device() {
    // py: test_all_known_product_ids_resolve_to_their_device
    for d in iter_known_devices() {
        for pid in d.product_ids {
            assert_eq!(resolve_device(Some(*pid), None), Some(d), "{pid:#06X}");
        }
        for w in d.wpids {
            assert_eq!(resolve_device_hidpp(None, Some(*w), None), Some(d));
        }
    }
}

#[test]
fn test_all_device_layout_keys_resolve_to_button_sets() {
    // py: test_all_device_layout_keys_resolve_to_button_sets
    for d in iter_known_devices() {
        assert!(get_buttons_for_layout(d.ui_layout).is_some(), "{}", d.key);
    }
}

#[test]
fn test_build_connected_device_info_uses_registry_defaults() {
    // py: test_build_connected_device_info_uses_registry_defaults
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB023),
        product_name: Some("MX Master 3 for Mac"),
        transport: Some("Bluetooth Low Energy"),
        source: Some("iokit-enumerate"),
        ..Default::default()
    });
    assert_eq!(info.display_name, "MX Master 3");
    assert_eq!(info.product_id, Some(0xB023));
    assert_eq!(info.transport.as_deref(), Some("Bluetooth Low Energy"));
    assert_eq!(info.gesture_cids, DEFAULT_GESTURE_CIDS);
    assert_eq!(info.ui_layout, "mx_master_3");
    assert!(has(&info.supported_buttons, "mode_shift"));
}

#[test]
fn test_hid_capability_guardrail_filters_runtime_buttons() {
    // py: test_hid_capability_guardrail_filters_runtime_buttons
    let controls = [bare(0x52), bare(0x53), bare(0x56), bare(0xC3)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB023),
        reprog_controls: &controls,
        gesture_cids: Some(&[0xC3]),
        ..Default::default()
    });
    assert!(has(&info.supported_buttons, "gesture"));
    assert!(!has(&info.supported_buttons, "mode_shift"));
    assert!(has(&info.supported_buttons, "hscroll_left"));
}

#[test]
fn test_build_mx_anywhere_3s_uses_exact_catalog_layout() {
    // py: test_build_mx_anywhere_3s_uses_exact_catalog_layout
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB037),
        ..Default::default()
    });
    let layout = get_device_layout(Some(info.ui_layout));
    assert_eq!((&*info.key, info.ui_layout), ("mx_anywhere_3s", "mx_anywhere_3s"));
    assert_eq!(layout.key, "mx_anywhere_3s");
    assert!(layout.interactive);
}

#[test]
fn test_build_connected_device_info_falls_back_to_runtime_name() {
    // py: test_build_connected_device_info_falls_back_to_runtime_name
    let controls = [bare(0xC3)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB999),
        product_name: Some("Mystery Logitech Mouse"),
        reprog_controls: &controls,
        gesture_cids: Some(&[0xF1]),
        ..Default::default()
    });
    assert_eq!(info.display_name, "Mystery Logitech Mouse");
    assert_eq!(info.key, "mystery_logitech_mouse");
    assert_eq!(info.gesture_cids, [0xF1]);
    assert_eq!(info.ui_layout, "generic_mouse");
    assert_eq!(info.image_asset, "icons/mouse-simple.svg");
    assert_eq!(info.supported_buttons, ["middle", "xbutton1", "xbutton2"]);
}

#[test]
fn test_m720_resolves_to_full_support() {
    // py: test_m720_resolves_to_full_support
    let controls: Vec<_> = [0x50, 0x51, 0x52, 0x53, 0x56, 0x5B, 0x5D, 0xD0, 0xD7].map(bare).to_vec();
    for pid in [0xC52B, 0xB015] {
        let info = build_connected_device_info(&ConnectedDeviceParams {
            product_id: Some(pid),
            product_name: Some("M720 Triathlon Multi-Device Mouse"),
            reprog_controls: &controls,
            ..Default::default()
        });
        assert_eq!(info.key, "m720_triathlon");
        assert_eq!(info.display_name, "M720 Triathlon Multi-Device Mouse");
        assert_eq!(info.ui_layout, "m720_triathlon");
        assert_eq!(info.image_asset, "logitech-mice/m720_triathlon/mouse.svg");
        assert_eq!(info.supported_buttons, M720_BUTTONS);
    }
}

#[test]
fn test_m720_resolves_by_documented_bluetooth_pairing_name() {
    // py: test_m720_resolves_by_documented_bluetooth_pairing_name
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB015),
        product_name: Some("M720 Triathlon Mouse"),
        ..Default::default()
    });
    assert_eq!((&*info.key, info.ui_layout), ("m720_triathlon", "m720_triathlon"));
}

#[test]
fn test_m720_catalog_declares_verified_controls_and_fixed_dpi() {
    // py: test_m720_catalog_declares_verified_controls_and_fixed_dpi
    let d = resolve_device(Some(0xB015), None).unwrap();
    assert_eq!(d.gesture_cids, [0xD0, 0xD7]);
    assert_eq!(d.supported_buttons, M720_BUTTONS);
    assert_eq!((d.dpi_min, d.dpi_max), (1000, 1000));
    assert!(d.aliases.contains(&"M720 Triathlon Mouse"));
}

#[test]
fn test_m720_bare_receiver_pid_without_name_falls_back_to_generic() {
    // py: test_m720_bare_receiver_pid_without_name_falls_back_to_generic
    let controls = [bare(0x52), bare(0x53), bare(0x56)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        reprog_controls: &controls,
        ..Default::default()
    });
    assert_ne!(info.key, "m720_triathlon");
    assert_eq!(info.ui_layout, "generic_mouse");
    assert_eq!(info.image_asset, "icons/mouse-simple.svg");
    assert_eq!(info.supported_buttons, GENERIC_BUTTONS);
    assert_eq!(info.display_name, "Logitech PID 0xC52B");
}

#[test]
fn test_resolve_g502_hero_by_product_id() {
    // py: test_resolve_g502_hero_by_product_id
    let d = resolve_device(Some(0xC08B), None).unwrap();
    assert_eq!((d.key, d.ui_layout), ("g502_hero", "g502"));
}

#[test]
fn test_resolve_g502_lightspeed_by_receiver_wpid() {
    // py: test_resolve_g502_lightspeed_by_receiver_wpid
    for pid in [0xC08D, 0x407F] {
        assert_eq!(key(pid), "g502_lightspeed");
    }
}

#[test]
fn test_resolve_g502_x_plus_by_alias() {
    // py: test_resolve_g502_x_plus_by_alias
    assert_eq!(by_name("G502 X PLUS").key, "g502_x");
}

#[test]
fn test_resolve_g502_x_plus_by_product_id() {
    // py: test_resolve_g502_x_plus_by_product_id
    for pid in [0xC095, 0x4099] {
        assert_eq!(key(pid), "g502_x");
    }
}

#[test]
fn test_resolve_g502_proteus_by_reported_name() {
    // py: test_resolve_g502_proteus_by_reported_name
    assert_eq!(by_name("Tunable FPS Gaming Mouse G502").key, "g502");
}

#[test]
fn test_g502_buttons_are_os_level_only() {
    // py: test_g502_buttons_are_os_level_only
    let b = get_buttons_for_layout("g502").unwrap();
    for k in ["middle", "xbutton1", "xbutton2", "hscroll_left", "hscroll_right"] {
        assert!(has(b, k), "{k}");
    }
    for k in ["gesture", "mode_shift", "dpi_switch"] {
        assert!(!has(b, k), "{k}");
    }
}

#[test]
fn test_g502_layout_is_placeholder_not_generic() {
    // py: test_g502_layout_is_placeholder_not_generic
    let l = get_device_layout(Some("g502"));
    assert_eq!(l.key, "g502");
    assert!(!l.interactive);
    assert!(l.hotspots.is_empty());
}

#[test]
fn test_build_g502_hero_connected_info() {
    // py: test_build_g502_hero_connected_info
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC08B),
        product_name: Some("G502 HERO Gaming Mouse"),
        transport: Some("usb"),
        ..Default::default()
    });
    assert_eq!(info.key, "g502_hero");
    assert_eq!(info.display_name, "G502 HERO");
    assert_eq!(info.ui_layout, "g502");
    assert_eq!(clamp_dpi(50000, Some(&info)), 25600);
    assert_eq!(clamp_dpi(50, Some(&info)), 100);
}

#[test]
fn test_known_device_layout_metadata_is_valid() {
    // py: test_known_device_layout_metadata_is_valid
    for d in iter_known_devices() {
        assert!(!d.ui_layout.ends_with(".png") && !d.ui_layout.ends_with(".svg"));
        assert!(get_buttons_for_layout(d.ui_layout).is_some());
        assert_ne!(get_device_layout(Some(d.ui_layout)).key, "generic_mouse", "{}", d.key);
    }
}

#[test]
fn test_clamp_dpi_uses_known_device_bounds() {
    // py: test_clamp_dpi_uses_known_device_bounds
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB019),
        ..Default::default()
    });
    assert_eq!(clamp_dpi(8000, Some(&info)), 4000);
    assert_eq!(clamp_dpi(100, Some(&info)), 200);
}

#[test]
fn test_clamp_dpi_defaults_without_device() {
    // py: test_clamp_dpi_defaults_without_device
    assert_eq!(clamp_dpi(100, None), 200);
    assert_eq!(clamp_dpi(9000, None), 8000);
}

#[test]
fn test_clamp_dpi_str_follows_python_int_base_zero() {
    // py: clamp_dpi docstring contract (hex strings coerce, malformed returns the floor)
    assert_eq!(clamp_dpi_str("0x5DC", None), 1500);
    assert_eq!(clamp_dpi_str(" 1200 ", None), 1200);
    assert_eq!(clamp_dpi_str("0b1111101000", None), 1000);
    assert_eq!(clamp_dpi_str("abc", None), 200);
    assert_eq!(clamp_dpi_str("", None), 200);
    assert_eq!(clamp_dpi_str("0800", None), 200); // int("0800", 0) is a ValueError
    assert_eq!(clamp_dpi_str("-5", None), 200);
    assert_eq!(clamp_dpi_str("99999", None), 8000);
}

#[test]
fn test_mx_master_4_has_actions_ring_button() {
    // py: test_mx_master_4_has_actions_ring_button
    assert!(has(resolve_device(Some(0xB042), None).unwrap().supported_buttons, "actions_ring"));
}

#[test]
fn test_mx_master_4_buttons_superset_of_mx_master() {
    // py: test_mx_master_4_buttons_superset_of_mx_master
    assert!(MX_MASTER_BUTTONS.iter().all(|b| has(MX_MASTER_4_BUTTONS, b)));
}

#[test]
fn test_mx_master_3s_lacks_actions_ring() {
    // py: test_mx_master_3s_lacks_actions_ring
    assert!(!has(resolve_device(Some(0xB034), None).unwrap().supported_buttons, "actions_ring"));
}

#[test]
fn test_mx_master_4_build_info_has_actions_ring() {
    // py: test_mx_master_4_build_info_has_actions_ring
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB042),
        product_name: Some("MX Master 4"),
        transport: Some("Bluetooth Low Energy"),
        source: Some("hidapi-enumerate"),
        ..Default::default()
    });
    assert!(has(&info.supported_buttons, "actions_ring"));
    assert_eq!(info.image_asset, "logitech-mice/mx_master_4/mouse.png");
    assert_eq!(info.ui_layout, "mx_master_4");
    assert!(info.gesture_via_sense_panel);
    assert_eq!(info.thumb_button_cid, Some(0xC3));
}

#[test]
fn test_mx_anywhere_2s_supported_buttons_include_middle_and_hscroll() {
    // py: test_mx_anywhere_2s_supported_buttons_include_middle_and_hscroll
    let d = resolve_device(Some(0xB01A), None).unwrap();
    for k in ["middle", "hscroll_left", "hscroll_right"] {
        assert!(has(d.supported_buttons, k));
    }
    assert!(!has(d.supported_buttons, "mode_shift"));
}

#[test]
fn test_get_buttons_for_mx_anywhere_2s_layout_uses_specific_tuple() {
    // py: test_get_buttons_for_mx_anywhere_2s_layout_uses_specific_tuple
    let d = resolve_device(Some(0xB01A), None).unwrap();
    assert!(core::ptr::eq(get_buttons_for_layout("mx_anywhere_2s").unwrap(), d.supported_buttons));
}

#[test]
fn test_catalog_has_all_17_mouser_devices() {
    // py: KNOWN_LOGI_DEVICES (LOGI_DEVICE_SPECS 16 + mx_vertical)
    let keys: Vec<_> = iter_known_devices().map(|d| d.key).collect();
    assert_eq!(
        keys,
        [
            "mx_master_4", "mx_master_3s", "mx_master_3", "mx_master_2s", "mx_master",
            "mx_anywhere_3s", "mx_anywhere_3", "mx_anywhere_2s", "mx_anywhere_2", "m650",
            "m585_m590", "m720_triathlon", "g502_hero", "g502_lightspeed", "g502_x", "g502",
            "mx_vertical"
        ]
    );
    let v = by_name("MX Vertical");
    assert_eq!((v.product_ids, v.dpi_max, v.supported_buttons), (&[0xB020][..], 4000, MX_VERTICAL_BUTTONS));
}

// ---- RuntimeSupportedButtonTests ----

fn derive(
    buttons: &[&'static str],
    controls: &[ReprogControlDetail],
    gesture: Option<&[u16]>,
    active: Option<u16>,
    rawxy: Option<bool>,
) -> Vec<&'static str> {
    derive_supported_buttons_from_reprog_controls(buttons, controls, gesture, active, rawxy)
}

#[test]
fn test_reprog_control_filter_keeps_static_buttons_without_controls() {
    // py: test_reprog_control_filter_keeps_static_buttons_without_controls
    assert_eq!(derive(MX_MASTER_BUTTONS, &[], Some(&[0xC3]), None, None), MX_MASTER_BUTTONS);
}

#[test]
fn test_capability_inventory_serializes_json_safe_shape() {
    // py: test_capability_inventory_serializes_json_safe_shape
    let controls = [ctl(0xC4, 0x0531), ctl(0xD7, 0x03A0), ctl(0x1A0, 0x0531)];
    let features = [
        HidppFeatureInfo::new(0x1B04, 0, 4),
        HidppFeatureInfo::new(0x2201, 1, 0),
        HidppFeatureInfo::new(0x2111, 2, 0),
        HidppFeatureInfo::new(0x1004, 3, 0),
    ];
    let inv = build_device_capability_inventory(
        &controls,
        &InventoryParams {
            gesture_cids: Some(&[0xD7]),
            active_gesture_cid: Some(0xD7),
            gesture_rawxy_enabled: Some(true),
            discovered_features: &features,
            ..Default::default()
        },
    );
    assert!(inv.has_reprog_controls);
    assert_eq!(inv.active_gesture_cid, Some(0xD7));
    assert!(inv.gesture_directions && inv.mode_shift && inv.smart_shift);
    assert!(inv.adjustable_dpi && inv.battery);
    assert!(inv.known_unsupported_controls.iter().all(|(c, _)| *c != 0x1A0));
    let j = inv.to_json();
    assert_eq!(j["known_unsupported_controls"], serde_json::json!([]));
    assert_eq!(j["active_gesture_cid"], "0x00D7");
    assert_eq!(j["control_cids"], serde_json::json!(["0x00C4", "0x00D7", "0x01A0"]));
}

#[test]
fn test_reprog_control_filter_removes_missing_gesture_group() {
    // py: test_reprog_control_filter_removes_missing_gesture_group
    let c = [bare(0x52), bare(0x53), bare(0x56), bare(0xC4)];
    let b = derive(MX_MASTER_BUTTONS, &c, Some(&[0xC3]), None, None);
    for k in ["gesture", "gesture_left", "gesture_right"] {
        assert!(!has(&b, k));
    }
}

#[test]
fn test_reprog_control_filter_keeps_selected_gesture_cid() {
    // py: test_reprog_control_filter_keeps_selected_gesture_cid
    let c = [ctl(0xD7, 0x03B0), ctl(0xC4, 0x0130)];
    let b = derive(MX_MASTER_BUTTONS, &c, Some(&[0xD7]), Some(0xD7), Some(true));
    assert!(has(&b, "gesture") && has(&b, "gesture_up"));
}

#[test]
fn test_reprog_control_filter_removes_directional_gestures_without_rawxy() {
    // py: test_reprog_control_filter_removes_directional_gestures_without_rawxy
    let c = [ctl(0xC3, 0x0030), ctl(0xC4, 0x0130)];
    let b = derive(MX_MASTER_BUTTONS, &c, Some(&[0xC3]), Some(0xC3), Some(false));
    assert!(has(&b, "gesture"));
    for k in ["gesture_left", "gesture_right", "gesture_up", "gesture_down"] {
        assert!(!has(&b, k));
    }
}

#[test]
fn test_reprog_control_filter_removes_missing_mode_shift() {
    // py: test_reprog_control_filter_removes_missing_mode_shift
    let c = [ctl(0xC3, 0x0130), bare(0x52)];
    assert!(!has(&derive(MX_MASTER_BUTTONS, &c, Some(&[0xC3]), Some(0xC3), Some(true)), "mode_shift"));
}

#[test]
fn test_reprog_control_filter_removes_non_divertable_mode_shift() {
    // py: test_reprog_control_filter_removes_non_divertable_mode_shift
    let c = [ctl(0xC3, 0x0130), ctl(0xC4, 0x0110)];
    assert!(!has(&derive(MX_MASTER_BUTTONS, &c, Some(&[0xC3]), Some(0xC3), Some(true)), "mode_shift"));
}

#[test]
fn test_reprog_control_filter_removes_missing_dpi_switch() {
    // py: test_reprog_control_filter_removes_missing_dpi_switch
    let c = [bare(0x52), bare(0x53), bare(0x56)];
    assert!(!has(&derive(MX_VERTICAL_BUTTONS, &c, None, None, None), "dpi_switch"));
}

#[test]
fn test_reprog_control_filter_keeps_dpi_switch_when_divertable() {
    // py: (complement of the missing-dpi_switch test; CID 0xFD gates it)
    let c = [bare(0x52), ctl(0xFD, 0x0130)];
    assert!(has(&derive(MX_VERTICAL_BUTTONS, &c, None, None, None), "dpi_switch"));
}

#[test]
fn test_reprog_control_filter_preserves_hscroll_without_hscroll_cids() {
    // py: test_reprog_control_filter_preserves_hscroll_without_hscroll_cids
    let c = [ctl(0xC3, 0x0130), ctl(0xC4, 0x0130)];
    let b = derive(MX_MASTER_BUTTONS, &c, Some(&[0xC3]), Some(0xC3), Some(true));
    assert!(has(&b, "hscroll_left") && has(&b, "hscroll_right"));
}

#[test]
fn test_reprog_control_filter_ignores_unknown_cids_and_preserves_order() {
    // py: test_reprog_control_filter_ignores_unknown_cids_and_preserves_order
    let c = [ctl(0x1A0, 0x0130), ctl(0xC3, 0x0130), ctl(0xC4, 0x0130)];
    let b = derive(MX_MASTER_BUTTONS, &c, Some(&[0xC3]), Some(0xC3), Some(true));
    assert_eq!(b, MX_MASTER_BUTTONS);
}

#[test]
fn test_mx_anywhere_2s_solaar_controls_keep_tilt_hscroll_without_mode_shift() {
    // py: test_mx_anywhere_2s_solaar_controls_keep_tilt_hscroll_without_mode_shift
    let c = [
        ctl(0x52, 0x130), ctl(0x53, 0x130), ctl(0x56, 0x130),
        ctl(0x5B, 0x130), ctl(0x5D, 0x130), ctl(0xD7, 0x03A0),
    ];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB01A),
        reprog_controls: &c,
        gesture_cids: Some(&[0xD7]),
        active_gesture_cid: Some(0xD7),
        gesture_rawxy_enabled: Some(true),
        ..Default::default()
    });
    for k in ["gesture_left", "gesture_right", "hscroll_left", "hscroll_right"] {
        assert!(has(&info.supported_buttons, k), "{k}");
    }
    assert!(!has(&info.supported_buttons, "mode_shift"));
}

#[test]
fn test_m720_issue_47_full_support_with_runtime_gesture() {
    // py: test_m720_issue_47_full_support_with_runtime_gesture
    let c = [
        ctl(0x52, 0x171), ctl(0x53, 0x171), ctl(0x56, 0x171), ctl(0x5B, 0x171),
        ctl(0x5D, 0x171), ctl(0xD0, 0x171), ctl(0xD7, 0x03A0),
    ];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB015),
        product_name: Some("M720_Triathlon"),
        reprog_controls: &c,
        gesture_cids: Some(&[0xD0]),
        active_gesture_cid: Some(0xD0),
        gesture_rawxy_enabled: Some(true),
        discovered_features: &[HidppFeatureInfo::new(0x1B04, 0, 4), HidppFeatureInfo::new(0x1000, 1, 0)],
        ..Default::default()
    });
    assert_eq!((&*info.key, info.ui_layout), ("m720_triathlon", "m720_triathlon"));
    assert_eq!(info.supported_buttons, M720_BUTTONS);
    assert_eq!(info.capability_inventory.active_gesture_cid, Some(0xD0));
    assert_eq!(info.capability_inventory.hscroll_cids, [0x5B, 0x5D]);
    assert!(!has(&info.supported_buttons, "mode_shift"));
}

#[test]
fn test_mx_ergo_issue_22_reports_precision_mode_without_support_claim() {
    // py: test_mx_ergo_issue_22_reports_precision_mode_without_support_claim
    let c = [
        ctl(0x52, 0x171), ctl(0x53, 0x171), ctl(0x56, 0x171), ctl(0xED, 0x171),
        ctl(0x5B, 0x171), ctl(0x5D, 0x171), ctl(0xD7, 0x03A0),
    ];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB01D),
        product_name: Some("MX Ergo Multi-Device Trackball"),
        reprog_controls: &c,
        ..Default::default()
    });
    assert_eq!(info.key, "mx_ergo_multi-device_trackball");
    assert_eq!(info.ui_layout, "generic_mouse");
    assert_eq!(info.supported_buttons, GENERIC_BUTTONS);
    assert_eq!(info.capability_inventory.hscroll_cids, [0x5B, 0x5D]);
    assert!(!has(&info.supported_buttons, "mode_shift"));
    assert_eq!(
        info.capability_inventory.to_json()["known_unsupported_controls"],
        serde_json::json!([{"cid": "0x00ED", "name": "precision_mode"}])
    );
}

#[test]
fn test_ergo_m575_issue_95_reports_inventory_without_support_claim() {
    // py: test_ergo_m575_issue_95_reports_inventory_without_support_claim
    let c = [ctl(0x52, 0x571), ctl(0x56, 0x571), ctl(0x53, 0x571), ctl(0xD7, 0x03A0)];
    let features = [
        HidppFeatureInfo::new(0x1B04, 0, 4),
        HidppFeatureInfo::new(0x2201, 1, 0),
        HidppFeatureInfo::new(0x1004, 2, 0),
    ];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        product_name: Some("ERGO M575 Trackball"),
        reprog_controls: &c,
        discovered_features: &features,
        ..Default::default()
    });
    assert_eq!(info.key, "ergo_m575_trackball");
    assert_eq!(info.ui_layout, "generic_mouse");
    assert_eq!(info.supported_buttons, GENERIC_BUTTONS);
    assert!(!has(&info.supported_buttons, "gesture") && !has(&info.supported_buttons, "hscroll_left"));
    assert!(info.capability_inventory.gesture_click);
    assert!(info.capability_inventory.adjustable_dpi);
}

#[test]
fn test_mx_anywhere_3s_solaar_controls_keep_mode_shift_and_catalog_hscroll() {
    // py: test_mx_anywhere_3s_solaar_controls_keep_mode_shift_and_catalog_hscroll
    // Changed for #142: no gesture_* buttons on the 3S.
    let c = [ctl(0x52, 0x130), ctl(0x53, 0x130), ctl(0x56, 0x130), ctl(0xC4, 0x130), ctl(0xD7, 0x03A0)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB037),
        reprog_controls: &c,
        gesture_cids: Some(&[0xD7]),
        active_gesture_cid: Some(0xD7),
        gesture_rawxy_enabled: Some(true),
        ..Default::default()
    });
    for k in ["mode_shift", "hscroll_left", "hscroll_right"] {
        assert!(has(&info.supported_buttons, k), "{k}");
    }
    assert!(!has(&info.supported_buttons, "gesture_up"));
}

#[test]
fn test_mx_master_4_haptic_control_does_not_create_supported_button() {
    // py: test_mx_master_4_haptic_control_does_not_create_supported_button
    let c = [
        ctl(0x52, 0x130), ctl(0x53, 0x130), ctl(0x56, 0x130), ctl(0xC3, 0x130),
        ctl(0xC4, 0x130), ctl(0x1A0, 0x130), ctl(0xD7, 0x03A0),
    ];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB042),
        reprog_controls: &c,
        gesture_cids: Some(&[0xC3, 0xD7]),
        active_gesture_cid: Some(0xC3),
        gesture_rawxy_enabled: Some(true),
        ..Default::default()
    });
    assert!(has(&info.supported_buttons, "mode_shift"));
    assert!(has(&info.supported_buttons, "gesture_down"));
    assert!(!has(&info.supported_buttons, "action_ring") && !has(&info.supported_buttons, "haptic"));
    assert!(info.capability_inventory.known_unsupported_controls.iter().all(|(c, _)| *c != 0x1A0));
}

// ---- hid inventory / diagnostics ----

#[test]
fn test_inventory_exposes_identity_features_controls_and_wheels() {
    // py: tests/test_hid_inventory.py::test_inventory_exposes_identity_features_controls_and_wheels
    let controls = [
        ReprogControlDetail {
            index: Some(0), cid: 0xC4, task: Some(0x9D), flags: Some(0x0531), pos: Some(5),
            group: Some(2), gmask: Some(0x0F), mapped_to: Some(0xC4), mapping_flags: Some(0x0011),
        },
        ReprogControlDetail {
            index: Some(1), cid: 0xD7, task: Some(0xB4), flags: Some(0x03A0),
            mapped_to: Some(0xD7), mapping_flags: Some(0), ..Default::default()
        },
    ];
    let features = [
        HidppFeatureInfo::new(0x1B04, 0x09, 4),
        HidppFeatureInfo { feature_id: 0x2111, index: Some(0x0E), ..Default::default() },
        HidppFeatureInfo { feature_id: 0x2121, index: Some(0x10), ..Default::default() },
        HidppFeatureInfo { feature_id: 0x2201, index: Some(0x0D), ..Default::default() },
    ];
    let inv = build_device_capability_inventory(
        &controls,
        &InventoryParams {
            device_identity: &[("product_id", "0xB034"), ("product_name", "MX Master 3S"), ("backend", "iokit")],
            gesture_cids: Some(&[0xD7]),
            active_gesture_cid: Some(0xD7),
            gesture_rawxy_enabled: Some(true),
            discovered_features: &features,
            ..Default::default()
        },
    );
    let d = inv.to_json();
    assert_eq!(d["device_identity"]["product_id"], "0xB034");
    assert_eq!(d["device_identity"]["backend"], "iokit");
    assert!(d["raw_features"].as_array().unwrap().contains(&serde_json::json!(
        {"feature_id": "0x1B04", "name": "REPROG_CONTROLS_V4", "index": "0x09", "version": 4}
    )));
    let c = &d["reprog_control_details"].as_array().unwrap().iter()
        .find(|c| c["cid"] == "0x00C4").unwrap().clone();
    assert_eq!(c["divertable"], true);
    assert_eq!(c["raw_xy_support"], true);
    assert_eq!(c["diverted"], true);
    assert_eq!(c["position"], 5);
    assert_eq!(c["group_mask"], "0x0F");
    assert_eq!(c["task"], "0x009D");
    let present = |id: &str| d["wheel_features"].as_array().unwrap().iter()
        .find(|w| w["feature_id"] == id).unwrap()["present"].clone();
    assert_eq!(present("0x2111"), true);
    assert_eq!(present("0x2121"), true);
    assert_eq!(present("0x2150"), false);
}

#[test]
fn test_feature_list_is_sorted_deduped_and_named() {
    // py: tests/test_hid_inventory.py::test_feature_dict_shape_is_accepted_for_legacy_dump_inputs
    // (Rust input is structured; this checks the same output: sorted, first duplicate wins, names).
    let features = [
        HidppFeatureInfo::new(0x2110, 0x0E, 0),
        HidppFeatureInfo::new(0x1B04, 0x09, 4),
        HidppFeatureInfo::new(0x2110, 0x55, 0),
        HidppFeatureInfo::new(0x9999, 0x01, 0),
    ];
    let inv = build_device_capability_inventory(
        &[],
        &InventoryParams { discovered_features: &features, ..Default::default() },
    );
    let ids: Vec<_> = inv.raw_features.iter().map(|f| f.feature_id).collect();
    assert_eq!(ids, [0x1B04, 0x2110, 0x9999]);
    assert_eq!(inv.raw_features[1].index, Some(0x0E));
    assert_eq!(inv.raw_features[2].name, "UNKNOWN");
    assert_eq!(inv.to_json()["raw_features"][0]["feature_id"], "0x1B04");
    assert!(inv.wheel_features[0].present && inv.smart_shift);
}

#[test]
fn test_inventory_adds_reprog_unavailable_diagnostic_without_controls() {
    // py: tests/test_hid_diagnostics.py::test_inventory_adds_reprog_unavailable_diagnostic_without_controls
    let j = build_device_capability_inventory(&[], &InventoryParams::default()).to_json();
    assert!(j["diagnostics"].as_array().unwrap().contains(&serde_json::json!({
        "code": "reprog_controls_unavailable",
        "severity": "info",
        "message": "REPROG_CONTROLS_V4 was not discovered; runtime remap evidence is limited."
    })));
}

#[test]
fn test_explicit_diagnostics_are_serialized() {
    // py: tests/test_hid_diagnostics.py::test_explicit_diagnostics_are_serialized
    let diag = [DiagnosticBlocker {
        code: "missing_permissions".into(),
        severity: "warning".into(),
        message: "hidraw path is not readable by this user".into(),
    }];
    let j = build_device_capability_inventory(
        &[],
        &InventoryParams { diagnostics: &diag, ..Default::default() },
    )
    .to_json();
    assert!(j["diagnostics"].as_array().unwrap().contains(&diag[0].to_json()));
}

#[test]
fn test_reprog_control_flag_accessors() {
    // py: ReprogControlDetail properties (core/logi_devices.py:182-204)
    let c = ReprogControlDetail { cid: 1, flags: Some(0x03B0), mapping_flags: Some(1), ..Default::default() };
    assert!(c.divertable() && c.raw_xy_support() && c.force_raw_xy_support());
    assert!(c.is_virtual() && c.remappable() && c.diverted());
    let n = ReprogControlDetail::from_cid(1);
    assert!(!n.divertable() && !n.raw_xy_support() && !n.is_virtual() && !n.remappable() && !n.diverted());
}

#[test]
fn test_hires_thumb_runtime_overrides_catalog_tristate() {
    // py: _resolve_capability (None defers to catalog; non-None wins)
    let base = ConnectedDeviceParams { product_id: Some(0xB034), ..Default::default() };
    let i = build_connected_device_info(&base);
    assert!(i.has_hires_wheel && i.has_thumbwheel);
    let i = build_connected_device_info(&ConnectedDeviceParams {
        has_hires_wheel: Some(false),
        has_thumbwheel: Some(false),
        ..base
    });
    assert!(!i.has_hires_wheel && !i.has_thumbwheel);
}

#[test]
fn test_empty_gesture_cids_mean_no_gesture_not_defaults() {
    // Deviation from Mouser's `gesture_cids or DEFAULT` (needed for #142).
    let c = [ctl(0xC3, 0x130), ctl(0xD7, 0x03A0)];
    let none = build_device_capability_inventory(
        &c,
        &InventoryParams { gesture_cids: Some(&[]), ..Default::default() },
    );
    assert!(!none.gesture_click && none.active_gesture_cid.is_none());
    let dflt = build_device_capability_inventory(&c, &InventoryParams::default());
    assert!(dflt.gesture_click);
}

// ---- layouts (tests/test_device_layouts.py) ----

#[test]
fn test_known_devices_have_layouts_and_assets() {
    // py: test_known_devices_have_layouts_and_assets (asset-file existence is a GUI-crate concern)
    for d in iter_known_devices() {
        let l = get_device_layout(Some(d.ui_layout));
        assert_eq!(l.interactive, !l.hotspots.is_empty(), "{}", d.key);
        assert_eq!(l.key, d.ui_layout);
    }
}

#[test]
fn test_known_device_hotspots_are_supported_buttons() {
    // py: test_known_device_hotspots_are_supported_buttons
    for d in iter_known_devices() {
        for h in get_device_layout(Some(d.ui_layout)).hotspots {
            assert!(has(d.supported_buttons, h.button_key), "{}: {}", d.key, h.button_key);
        }
    }
}

#[test]
fn test_catalog_same_side_label_anchors_do_not_overlap() {
    // py: test_catalog_same_side_label_anchors_do_not_overlap (catalog layouts = all but the 4 family ones)
    for l in DEVICE_LAYOUTS.iter().skip(4) {
        for side in ["left", "right"] {
            let mut ys: Vec<f32> = l
                .hotspots
                .iter()
                .filter(|h| h.label_side == side)
                .map(|h| l.image_height as f32 * h.norm_y + h.label_off_y as f32)
                .collect();
            ys.sort_by(f32::total_cmp);
            for w in ys.windows(2) {
                assert!(w[1] - w[0] >= 35.0, "{} {side}: {:?}", l.key, w);
            }
        }
    }
}

#[test]
fn test_master_layout_is_interactive() {
    // py: test_master_layout_is_interactive
    let l = get_device_layout(Some("mx_master"));
    assert!(l.interactive && !l.hotspots.is_empty());
    assert_eq!(l.image_asset, "mouse.png");
}

#[test]
fn test_unknown_layout_falls_back_to_generic() {
    // py: test_unknown_layout_falls_back_to_generic
    for k in [Some("does_not_exist"), None, Some("")] {
        let l = get_device_layout(k);
        assert!(!l.interactive);
        assert_eq!(l.key, "generic_mouse");
        assert_eq!(l.image_asset, "icons/mouse-simple.svg");
    }
}

#[test]
fn test_family_fallback_chain_is_used_for_unlisted_device_keys() {
    // py: get_device_layout step 2 (_FAMILY_FALLBACKS): mx_anywhere_2 has no layout of its own
    assert_eq!(get_device_layout(Some("mx_anywhere_2")).key, "mx_anywhere");
}

#[test]
fn test_manual_choices_include_auto_and_interactive_layouts() {
    // py: test_manual_choices_include_auto_and_interactive_layouts
    let c: Vec<_> = get_manual_layout_choices().collect();
    assert_eq!(c[0], LayoutChoice { key: "", label: "Auto-detect" });
    for (k, l) in [("mx_master", "MX Master family"), ("mx_anywhere", "MX Anywhere family"), ("mx_vertical", "MX Vertical family")] {
        assert!(c.contains(&LayoutChoice { key: k, label: l }), "{k}");
    }
}

#[test]
fn test_manual_choices_include_gaming_family_layouts() {
    // py: test_manual_choices_include_gaming_family_layouts
    assert!(get_manual_layout_choices().any(|c| c.key == "g502" && c.label == "G502 family"));
}

#[test]
fn test_manual_choices_do_not_duplicate_layout_keys() {
    // py: test_manual_choices_do_not_duplicate_layout_keys
    let keys: Vec<_> = get_manual_layout_choices().map(|c| c.key).filter(|k| !k.is_empty()).collect();
    let mut u = keys.clone();
    u.sort_unstable();
    u.dedup();
    assert_eq!(keys.len(), u.len());
    assert!(keys.contains(&"mx_master_4")); // py: test_mx_master_4_in_manual_choices
}

#[test]
fn test_mx_anywhere_layout_is_interactive() {
    // py: test_mx_anywhere_layout_is_interactive
    let l = get_device_layout(Some("mx_anywhere"));
    assert!(l.interactive && !l.hotspots.is_empty());
    assert_eq!(l.image_asset, "mouse_mx_anywhere_3s.png");
}

#[test]
fn test_mx_anywhere_device_specific_keys_use_catalog_layout() {
    // py: test_mx_anywhere_device_specific_keys_use_catalog_layout
    for k in ["mx_anywhere_3s", "mx_anywhere_3"] {
        let l = get_device_layout(Some(k));
        assert_eq!(l.key, k);
        assert!(l.interactive && !l.hotspots.is_empty());
        assert_eq!(l.image_asset, format!("logitech-mice/{k}/mouse.png"));
    }
}

#[test]
fn test_mx_anywhere_2s_layout_identity_and_wheel_tilt_hotspots() {
    // py: test_mx_anywhere_2s_layout_identity_and_wheel_tilt_hotspots
    let l = get_device_layout(Some("mx_anywhere_2s"));
    assert_eq!((l.key, l.label), ("mx_anywhere_2s", "MX Anywhere 2S"));
    assert_eq!(l.image_asset, "logitech-mice/mx_anywhere_2s/mouse.png");
    assert_eq!(layout_keys(l), ["middle", "xbutton1", "xbutton2", "hscroll_left"]);
    let hs = l.hotspots[3];
    assert_eq!((hs.label, hs.summary_type, hs.is_hscroll), ("Horizontal scroll", "hscroll", true));
    assert_eq!(l.hotspots[1].label, "Back button");
}

#[test]
fn test_mx_anywhere_2s_has_no_self_fallback() {
    // py: test_mx_anywhere_2s_has_no_self_fallback
    assert!(FAMILY_FALLBACKS.contains(&("mx_anywhere_2s", "mx_anywhere")));
}

#[test]
fn test_mx_vertical_layout_is_interactive() {
    // py: test_mx_vertical_layout_is_interactive
    let l = get_device_layout(Some("mx_vertical"));
    assert!(l.interactive && !l.hotspots.is_empty());
    assert_eq!(l.image_asset, "mx_vertical.png");
}

#[test]
fn test_mx_master_4_layout_has_actions_ring_and_own_layout() {
    // py: test_mx_master_4_has_actions_ring_hotspot / test_mx_master_4_no_longer_falls_back
    // / test_exact_mx_master_4_layout_uses_catalog_asset
    let l = get_device_layout(Some("mx_master_4"));
    assert!(layout_keys(l).contains(&"actions_ring"));
    assert_eq!(l.key, "mx_master_4");
    assert!(l.interactive);
    assert_eq!(l.image_asset, "logitech-mice/mx_master_4/mouse.png");
}

#[test]
fn test_exact_m585_m590_layout_uses_catalog_asset() {
    // py: test_exact_m585_m590_layout_uses_catalog_asset
    let l = get_device_layout(Some("m585_m590"));
    let mut k = layout_keys(l);
    k.sort_unstable();
    assert_eq!(k, ["hscroll_left", "hscroll_right", "middle", "xbutton1", "xbutton2"]);
    assert!(l.interactive);
    assert_eq!(l.image_asset, "logitech-mice/m585_m590/mouse.png");
}

#[test]
fn test_m720_layout_uses_project_schematic_and_control_anchors() {
    // py: test_m720_layout_uses_project_schematic_and_control_anchors
    let l = get_device_layout(Some("m720_triathlon"));
    assert!(l.interactive);
    assert_eq!(l.image_asset, "logitech-mice/m720_triathlon/mouse.svg");
    let at = |k: &str| {
        let h = l.hotspots.iter().find(|h| h.button_key == k).unwrap();
        (h.norm_x, h.norm_y, h.is_hscroll)
    };
    assert_eq!(l.hotspots.len(), 6);
    assert_eq!(at("middle"), (0.355, 0.240, false));
    assert_eq!(at("hscroll_left"), (0.327, 0.235, true));
    assert_eq!(at("hscroll_right"), (0.385, 0.246, true));
    assert_eq!(at("xbutton1"), (0.248, 0.551, false));
    assert_eq!(at("xbutton2"), (0.364, 0.516, false));
    assert_eq!(at("gesture"), (0.297, 0.721, false));
}

#[test]
fn test_exact_layouts_use_catalog_assets() {
    // py: test_exact_mx_master_3s_layout_uses_catalog_asset, test_exact_mx_anywhere_{2s,3,3s}_layout_uses_catalog_asset
    for (k, tilt, mode) in [
        ("mx_master_3s", true, true),
        ("mx_anywhere_2s", true, false),
        ("mx_anywhere_3", true, true),
        ("mx_anywhere_3s", true, true),
    ] {
        let l = get_device_layout(Some(k));
        let keys = layout_keys(l);
        assert!(l.interactive && l.key == k);
        assert_eq!(l.image_asset, format!("logitech-mice/{k}/mouse.png"));
        assert_eq!(keys.contains(&"hscroll_left"), tilt);
        assert_eq!(keys.contains(&"mode_shift"), mode);
    }
}

#[test]
fn test_layout_table_matches_mouser_counts_and_generic_index() {
    // py: DEVICE_LAYOUTS (4 base + 12 catalog)
    assert_eq!(DEVICE_LAYOUTS.len(), 16);
    assert_eq!(DEVICE_LAYOUTS[3].key, "generic_mouse");
    let total: usize = DEVICE_LAYOUTS.iter().map(|l| l.hotspots.len()).sum();
    assert_eq!(total, 70);
}

// ---- triage regressions ----

/// Controls from the #156 reporter (CIDs 0x50..0xD7, 0xD0 flags 0x0171).
fn m720_controls() -> Vec<ReprogControlDetail> {
    let mut v: Vec<_> = [0x50, 0x51, 0x52, 0x53, 0x56, 0x5B, 0x5D].map(|c| ctl(c, 0x0171)).to_vec();
    v.push(ctl(0xD0, 0x0171));
    v.push(ctl(0xD7, 0x03A0));
    v
}

#[test]
fn test_issue_156_receiver_pid_with_m720_name_resolves_to_m720_not_mx_master() {
    // triage #156: shared receiver PID + HID++ name must select the M720 entry
    let c = m720_controls();
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        product_name: Some("M720 Triathlon Multi-Device Mouse"),
        reprog_controls: &c,
        ..Default::default()
    });
    assert_eq!((&*info.key, info.ui_layout), ("m720_triathlon", "m720_triathlon"));
    assert_eq!(info.supported_buttons, M720_BUTTONS);
    assert_eq!(info.gesture_cids, [0xD0, 0xD7]); // 0xD0 is tried before 0xD7
    assert_eq!(info.capability_inventory.active_gesture_cid, Some(0xD0));
    assert!(!has(&info.supported_buttons, "mode_shift"));
}

#[test]
fn test_issue_156_receiver_pid_with_m720_wpid_resolves_without_name() {
    // triage #156: the HID++ WPID alone (0x405E) identifies the M720 behind the receiver
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        wpid: Some(0x405E),
        ..Default::default()
    });
    assert_eq!(info.key, "m720_triathlon");
}

#[test]
fn test_issue_156_unknown_name_gets_only_generic_buttons_from_live_controls() {
    // triage #156: same control list, unknown name -> no MX Master layout, no gesture/hscroll/mode_shift
    let c = m720_controls();
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        product_name: Some("Totally Unknown Mouse"),
        reprog_controls: &c,
        ..Default::default()
    });
    assert_eq!(info.ui_layout, "generic_mouse");
    assert_eq!(info.supported_buttons, GENERIC_BUTTONS);
    assert!(!info.supported_buttons.iter().any(|b| b.starts_with("gesture")));
}

#[test]
fn test_issue_156_every_shared_receiver_pid_never_resolves_a_model() {
    // triage #156: by construction, no receiver PID selects a catalog entry
    for pid in SHARED_RECEIVER_PIDS {
        assert!(resolve_device(Some(*pid), None).is_none(), "{pid:#06X}");
    }
}

#[test]
fn test_issue_142_mx_anywhere_3_mode_shift_without_gesture() {
    // triage #142 / py: test_mx_anywhere_3_issue_142_distinguishes_mode_shift_and_gesture (fixed)
    let c = [
        ctl(0x52, 0x0531), ctl(0x53, 0x0D31), ctl(0x56, 0x0D31),
        ctl(0xC4, 0x0531), ctl(0xD7, 0x03A0),
    ];
    let features = [
        HidppFeatureInfo::new(0x1B04, 1, 4),
        HidppFeatureInfo::new(0x2201, 2, 0),
        HidppFeatureInfo::new(0x2111, 3, 0),
        HidppFeatureInfo::new(0x1004, 4, 0),
    ];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB025),
        product_name: Some("MX Anywhere 3"),
        reprog_controls: &c,
        discovered_features: &features,
        ..Default::default()
    });
    assert_eq!(info.key, "mx_anywhere_3");
    assert!(has(&info.supported_buttons, "mode_shift"));
    assert!(!info.supported_buttons.iter().any(|b| b.starts_with("gesture")));
    assert_eq!(info.capability_inventory.active_gesture_cid, None);
    assert!(info.capability_inventory.mode_shift && info.capability_inventory.smart_shift);
    assert!(info.capability_inventory.adjustable_dpi);
    assert!(info.gesture_cids.is_empty());
}

#[test]
fn test_issue_142_mx_anywhere_3_wheel_mode_button_is_cid_c4() {
    // triage #142: mode_shift availability is tied to CID 0xC4 being divertable
    assert_eq!(CID_MODE_SHIFT, 0x00C4);
    let no_c4 = [ctl(0x52, 0x0531), ctl(0xD7, 0x03A0)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB025),
        reprog_controls: &no_c4,
        ..Default::default()
    });
    assert!(!has(&info.supported_buttons, "mode_shift"));
}

#[test]
fn test_issue_142_mx_anywhere_2_list_yields_only_buttons_it_has() {
    // triage #142: MX Anywhere 2 behind a receiver (name match) gets its catalog set narrowed by live controls
    let c = [ctl(0x52, 0x130), ctl(0x53, 0x130), ctl(0x56, 0x130)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        product_name: Some("Wireless Mouse MX Anywhere 2"),
        reprog_controls: &c,
        ..Default::default()
    });
    assert!(!info.supported_buttons.iter().any(|b| b.starts_with("gesture") || *b == "mode_shift"));
    assert!(has(&info.supported_buttons, "middle") && has(&info.supported_buttons, "xbutton1"));
}

#[test]
fn test_issue_214_smart_shift_feature_without_mode_shift_button() {
    // triage #214: feature 0x2111 and no CID 0xC4 -> smart_shift true, no mode_shift button
    let c = [ctl(0x52, 0x0531), ctl(0xD7, 0x03A0)];
    let features = [HidppFeatureInfo::new(0x2111, 3, 0)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB025),
        reprog_controls: &c,
        discovered_features: &features,
        ..Default::default()
    });
    assert!(info.capability_inventory.smart_shift);
    assert!(!has(&info.supported_buttons, "mode_shift"));
    assert!(info.smart_shift_available());
    assert!(!info.can_toggle_smart_shift());
}

#[test]
fn test_issue_214_anywhere_family_override_keeps_smart_shift_panel() {
    // triage #214: the "MX Anywhere family" override changes buttons only, never smart_shift
    let features = [HidppFeatureInfo::new(0x2111, 3, 0)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB025),
        discovered_features: &features,
        ..Default::default()
    });
    let eff = effective_supported_buttons(&info, "mx_anywhere");
    assert!(has(&eff, "mode_shift")); // no controls known: full family set
    assert_eq!(eff, MX_ANYWHERE_BUTTONS);
    assert!(info.smart_shift_available());
    // an unknown override key is ignored
    assert_eq!(effective_supported_buttons(&info, "nope"), info.supported_buttons);
}

#[test]
fn test_issue_214_no_smart_shift_feature_hides_panel_regardless_of_layout() {
    // triage #214: no 0x2110/0x2111 -> panel hidden even with an MX Master override
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xB025),
        discovered_features: &[HidppFeatureInfo::new(0x2201, 1, 0)],
        ..Default::default()
    });
    assert!(!info.smart_shift_available());
    assert!(has(&effective_supported_buttons(&info, "mx_master"), "mode_shift"));
    assert!(!info.smart_shift_available());
}

#[test]
fn test_issue_214_smart_shift_with_both_feature_ids() {
    // triage #214: 0x2110 (plain) and 0x2111 (enhanced) both count
    for id in [0x2110, 0x2111] {
        let inv = build_device_capability_inventory(
            &[],
            &InventoryParams { discovered_features: &[HidppFeatureInfo::new(id, 1, 0)], ..Default::default() },
        );
        assert!(inv.smart_shift, "{id:#06X}");
    }
}

#[test]
fn test_mx_master_2s_real_hardware_wpid_0x4069_on_unifying_receiver() {
    // D10 real hardware: MX Master 2S, WPID 0x4069, behind receiver 0xC52B
    let by_wpid = resolve_device_hidpp(Some(0xC52B), Some(0x4069), None).unwrap();
    assert_eq!(by_wpid.key, "mx_master_2s");
    let by_hidpp_name = resolve_device_hidpp(Some(0xC52B), None, Some("Wireless Mouse MX Master 2S")).unwrap();
    assert_eq!(by_hidpp_name, by_wpid);
    let controls = [
        ctl(0x50, 0x30), ctl(0x51, 0x30), ctl(0x52, 0x130), ctl(0x53, 0x130), ctl(0x56, 0x130),
        ctl(0xC3, 0x1B0), ctl(0xC4, 0x130),
    ];
    let features = [HidppFeatureInfo::new(0x2110, 5, 0), HidppFeatureInfo::new(0x2201, 6, 0)];
    let info = build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(0xC52B),
        wpid: Some(0x4069),
        reprog_controls: &controls,
        discovered_features: &features,
        ..Default::default()
    });
    assert_eq!((&*info.key, info.ui_layout), ("mx_master_2s", "mx_master_2s"));
    assert_eq!(info.display_name, "MX Master 2S");
    assert_eq!(info.image_asset, "logitech-mice/mx_master_2s/mouse.png");
    assert_eq!((info.dpi_min, info.dpi_max), (200, 4000));
    assert!(info.has_hires_wheel && info.has_thumbwheel);
    assert!(has(&info.supported_buttons, "mode_shift") && has(&info.supported_buttons, "gesture"));
    assert!(info.smart_shift_available() && info.can_toggle_smart_shift());
    assert_eq!(info.capability_inventory.device_identity.iter().find(|(k, _)| k == "wpid").unwrap().1, "0x4069");
    // the bare receiver PID alone must not select it
    assert!(resolve_device_hidpp(Some(0xC52B), None, None).is_none());
}

#[test]
fn test_family_override_offers_mode_shift_only_with_cid_c4() {
    // review fix: override is still gated by live CIDs
    let no_c4 = [ctl(0x52, 0x130)];
    let mut p = ConnectedDeviceParams { product_id: Some(0xB025), reprog_controls: &no_c4, ..Default::default() };
    let info = build_connected_device_info(&p);
    assert!(!has(&effective_supported_buttons(&info, "mx_anywhere"), "mode_shift"));
    let with_c4 = [ctl(0x52, 0x130), ctl(0xC4, 0x130)];
    p.reprog_controls = &with_c4;
    let info = build_connected_device_info(&p);
    assert!(has(&effective_supported_buttons(&info, "mx_anywhere"), "mode_shift"));
}

#[test]
fn test_name_matching_normalises_without_changing_results() {
    // review fix: zero-alloc matcher still honours case, underscores and whitespace runs
    assert_eq!(by_name("  mx__MASTER   2s ").key, "mx_master_2s");
    assert_eq!(by_name("Wireless Mouse MX Anywhere 2").key, "mx_anywhere_2");
    assert!(resolve_device(None, Some("   ")).is_none());
    assert!(resolve_device(None, Some("MX Master 2")).is_none());
}

#[test]
fn test_bolt_wpid_equal_to_model_pid_matches_and_mx_master_4_wpid() {
    // review fix: WPID also matches product_ids (Bolt), MX Master 4 WPID 0xB042
    assert_eq!(resolve_device_hidpp(Some(0xC548), Some(0xB034), None).unwrap().key, "mx_master_3s");
    assert_eq!(resolve_device_hidpp(Some(0xC548), Some(0xB042), None).unwrap().key, "mx_master_4");
    for pid in [0xC537, 0xC54D] {
        assert!(is_shared_receiver_pid(pid));
    }
    assert!(!is_shared_receiver_pid(0xC545));
}

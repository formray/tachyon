//! The version shown in Windows (file properties, winget) matches Cargo.toml.

#[test]
fn exe_version_info_and_packaging_match_cargo() {
    let v = env!("CARGO_PKG_VERSION");
    let rc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/res/app.rc")).unwrap();
    let commas = format!("{},0", v.replace('.', ","));
    assert!(rc.contains(&format!("FILEVERSION {commas}")), "res/app.rc FILEVERSION is not {v}");
    assert!(rc.contains(&format!("PRODUCTVERSION {commas}")), "res/app.rc PRODUCTVERSION is not {v}");
    assert!(rc.contains(&format!("\"FileVersion\", \"{v}.0\"")), "res/app.rc FileVersion is not {v}.0");
    assert!(rc.contains(&format!("\"ProductVersion\", \"{v}\"")), "res/app.rc ProductVersion is not {v}");
    let scoop = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/scoop/tachyon.json")).unwrap();
    assert!(scoop.contains(&format!("\"version\": \"{v}\"")), "packaging/scoop/tachyon.json is not {v}");
    let mcpb: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/mcpb/manifest.json")).unwrap())
            .unwrap();
    assert_eq!(mcpb["version"], v, "packaging/mcpb/manifest.json");
    assert_eq!(mcpb["user_config"]["config_file"]["required"], true, "the .mcpb must ask for the settings file");
    let reg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/mcp/server.json")).unwrap()).unwrap();
    assert_eq!(reg["version"], v, "packaging/mcp/server.json");
    assert_eq!(reg["packages"][0]["version"], v);
    let url = format!("https://github.com/formray/tachyon/releases/download/v{v}/tachyon-{v}-windows-x64.mcpb");
    assert_eq!(reg["packages"][0]["identifier"], url.as_str());
    let skill = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/skill/tachyon-gpu/SKILL.md")).unwrap();
    assert!(skill.contains(&format!("version: \"{v}\"")), "packaging/skill/tachyon-gpu/SKILL.md");
    let dir = format!("{}/packaging/winget/manifests/f/Formray/Tachyon/{v}", env!("CARGO_MANIFEST_DIR"));
    for f in ["Formray.Tachyon.yaml", "Formray.Tachyon.installer.yaml", "Formray.Tachyon.locale.en-US.yaml"] {
        let t = std::fs::read_to_string(format!("{dir}/{f}")).unwrap_or_else(|_| panic!("missing {dir}/{f}"));
        assert!(t.contains(&format!("PackageVersion: {v}")), "{f} is not {v}");
    }
}

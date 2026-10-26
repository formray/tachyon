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
    let dir = format!("{}/packaging/winget/manifests/f/Formray/Tachyon/{v}", env!("CARGO_MANIFEST_DIR"));
    for f in ["Formray.Tachyon.yaml", "Formray.Tachyon.installer.yaml", "Formray.Tachyon.locale.en-US.yaml"] {
        let t = std::fs::read_to_string(format!("{dir}/{f}")).unwrap_or_else(|_| panic!("missing {dir}/{f}"));
        assert!(t.contains(&format!("PackageVersion: {v}")), "{f} is not {v}");
    }
}

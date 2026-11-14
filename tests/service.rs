use super::*;

#[test]
fn python_yaml_names_one_application() {
    let one = "name: app\ntarget: app:app\nmodule_path: .\nvenv: null\n";
    assert!(serde_yaml::from_str::<Config>(one).is_ok());
    let many = "applications:\n  - target: app:app\n    module_path: .\n";
    assert!(serde_yaml::from_str::<Config>(many).is_err());
    // Its name in the application manager is declared, never assumed.
    let unnamed = "target: app:app\nmodule_path: .\n";
    assert!(serde_yaml::from_str::<Config>(unnamed).is_err());
}

/// Format the public build identity stamped into executable artifacts.
///
/// A missing commit is legitimate for crates.io/vendored source archives and
/// degrades to the package version. Builds made from a git checkout carry the
/// exact short commit and an explicit dirty marker when the checkout was not
/// clean at build time.
pub fn format_build_version(package_version: &str, commit: &str, dirty: bool) -> String {
    if commit == "unknown" {
        package_version.to_owned()
    } else if dirty {
        format!("{package_version}+g{commit}.dirty")
    } else {
        format!("{package_version}+g{commit}")
    }
}

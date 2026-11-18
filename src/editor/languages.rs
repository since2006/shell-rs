//! The highlighters ShellRS adds to gpui-kit's for what servers keep in
//! their configuration (issue #142): XML, Java's `.properties`, INI and
//! nginx's configuration. Each grammar is compiled C, as gpui-kit's are.

use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};

/// The names [`super::language_for`] gives them.
#[cfg(test)]
pub(super) const ADDED: [&str; 4] = ["xml", "properties", "ini", "nginx"];

/// Register them with gpui-kit, once the app starts. Again is harmless.
pub fn register_languages() {
    let registry = LanguageRegistry::singleton();
    for (name, language, highlights) in [
        (
            "xml",
            tree_sitter_xml::LANGUAGE_XML,
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
        ),
        (
            "properties",
            tree_sitter_properties::LANGUAGE,
            tree_sitter_properties::HIGHLIGHTS_QUERY,
        ),
        (
            "ini",
            tree_sitter_ini::LANGUAGE,
            tree_sitter_ini::HIGHLIGHTS_QUERY,
        ),
        (
            "nginx",
            tree_sitter_nginx::LANGUAGE,
            include_str!("nginx_highlights.scm"),
        ),
    ] {
        registry.register(
            name,
            &LanguageConfig::new(name, language.into(), Vec::new(), highlights, "", ""),
        );
    }
}

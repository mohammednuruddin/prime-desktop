//! Which product this build is. The name and ids come from `src/shared/product.json`, the one file a private build
//! replaces, so nothing else in the code names a product.

use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    /// Shown to the user: the window title, git's fallback author.
    pub name: String,
    /// The Tauri identifier. The WebView profile and, on Windows, the app's own files live under it.
    pub app_id: String,
    artifact_name: String,
}

impl Product {
    /// Lower-case and dashed: folders, sockets and pipes that must not hold spaces.
    pub fn slug(&self) -> String {
        self.artifact_name.to_lowercase()
    }

    /// The macOS folder name: the name without spaces.
    pub fn mac_dir(&self) -> String {
        self.name.split_whitespace().collect()
    }
}

pub fn product() -> &'static Product {
    static PRODUCT: OnceLock<Product> = OnceLock::new();
    PRODUCT.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../src/shared/product.json"))
            .expect("src/shared/product.json is invalid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_derive_from_the_product_file() {
        let product = product();
        assert!(!product.name.is_empty() && !product.app_id.is_empty());
        assert!(!product.slug().contains(char::is_whitespace));
        assert!(!product.mac_dir().contains(char::is_whitespace));
    }
}

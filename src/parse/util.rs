/// `NSC_ONLINE_CHECK_URL` overrides the URL; set but empty skips the check.
pub fn checkonline() -> bool {
    let url = std::env::var("NSC_ONLINE_CHECK_URL")
        .unwrap_or_else(|_| "https://nmcheck.gnome.org/check_network_status.txt".to_string());
    url.is_empty() || reqwest::blocking::get(url).is_ok()
}

use super::*;

static STALE_REFRESHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// After startup: rebuilds catalogues older than a day, then reloads once if one changed. Never
/// runs while loading.
pub(super) fn refresh_stale(input: relm4::Sender<AppMsg>) {
    use nsc_backend::sources;
    if STALE_REFRESHED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    relm4::spawn(async move {
        let mut changed = false;
        for s in crate::ui::sourceinfo::store().sources().into_iter().filter(|s| !s.builtin && s.enabled) {
            if !sources::stale(&s.id, sources::STALE_AFTER) {
                continue;
            }
            let link = sources::catalogue_link(&s.id);
            let before = std::fs::read_link(&link).ok();
            let _ = sources::fetch(&s.id, &s.flake, sources::FETCH_TIMEOUT).await;
            changed |= std::fs::read_link(&link).ok() != before;
        }
        if changed {
            input.emit(AppMsg::TryLoad);
        }
    });
}

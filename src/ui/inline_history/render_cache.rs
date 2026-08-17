//! One bounded, browser-owned rendering retained across list and scroll updates.

use super::*;
use crate::highlighter::Highlighter;
use std::sync::Mutex;

struct CachedDetail {
    detail: HistoryDetail,
    width: usize,
    compact: bool,
    theme: Option<Vec<u8>>,
    registry: Arc<LanguageRegistry>,
    lines: Arc<[RenderedTextLine]>,
}

/// Exact content comparison includes receipt changes and live saved-state labels.
/// Keeping only the latest detail bounds retained source and rendered data.
#[derive(Default)]
pub(crate) struct HistoryRenderCache(Mutex<Option<CachedDetail>>);

impl std::fmt::Debug for HistoryRenderCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HistoryRenderCache")
            .finish_non_exhaustive()
    }
}

impl HistoryRenderCache {
    #[cfg(test)]
    pub(crate) fn cached_lines(&self) -> Option<Arc<[RenderedTextLine]>> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|cached| Arc::clone(&cached.lines))
    }

    pub(super) fn render(
        &self,
        detail: &HistoryDetail,
        width: usize,
        compact: bool,
        theme: &Theme,
        registry: &Arc<LanguageRegistry>,
    ) -> Arc<[RenderedTextLine]> {
        let theme_key = serde_json::to_vec(theme).ok();
        {
            let cache = self.0.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(cached) = cache.as_ref() {
                if theme_key.is_some()
                    && cached.width == width
                    && cached.compact == compact
                    && cached.theme == theme_key
                    && Arc::ptr_eq(&cached.registry, registry)
                    && cached.detail == *detail
                {
                    return Arc::clone(&cached.lines);
                }
            }
        }
        let mut highlighter = Highlighter::with_registry(theme, Arc::clone(registry)).ok();
        let lines: Arc<[RenderedTextLine]> = detail
            .render(width, compact, theme, highlighter.as_mut())
            .into();
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(CachedDetail {
            detail: detail.clone(),
            width,
            compact,
            theme: theme_key,
            registry: Arc::clone(registry),
            lines: Arc::clone(&lines),
        });
        lines
    }
}

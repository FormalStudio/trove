//! What a tab carries for the search results, the columns view and its thumbnails, kept apart
//! from the listing itself.

use std::time::Duration;

use nd::prelude::*;

use crate::columns::Columns;
use crate::results::{Results, Scope};
use crate::tab::Tab;

pub struct TabExtras {
    pub results: Results,
    pub columns: Columns,
    /// The last thumbnail arrival this tab bumped its tiles for.
    pub thumbs_seen: u64,
    _thumbs: Subscription,
}

impl TabExtras {
    pub fn new(cx: &mut Context<Tab>) -> Self {
        let thumbs = crate::thumbs::get(cx);
        let watch = cx.observe(&thumbs, |tab: &mut Tab, thumbs, cx| crate::thumbs::arrived(tab, &thumbs, cx));
        Self { results: Results::new(cx), columns: Columns::new(cx), thumbs_seen: 0, _thumbs: watch }
    }
}

impl Tab {
    pub fn results(&self) -> &Results {
        &self.extras.results
    }

    pub fn set_search_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        crate::results::set_scope(self, scope, cx);
    }

    /// Applies what the search and the columns' listings sent: the host's pump, for tests.
    pub fn drain_extras(&mut self, timeout: Duration, cx: &mut Context<Self>) {
        self.extras.results.drain(timeout, cx);
        self.drain_columns(timeout, cx);
    }
}

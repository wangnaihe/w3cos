use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::selector_filter::AncestorBloom;
use super::{Combinator, CompoundSelector};

pub(super) struct ParsedSelectorChain {
    pub(super) chain: Vec<CompoundSelector>,
    pub(super) combinators: Vec<Combinator>,
    pub(super) ancestor_filter: AncestorBloom,
}

impl ParsedSelectorChain {
    fn new(chain: Vec<CompoundSelector>, combinators: Vec<Combinator>) -> Self {
        Self {
            ancestor_filter: AncestorBloom::for_rule(&chain, &combinators),
            chain,
            combinators,
        }
    }
}

pub(super) const SELECTOR_PARSE_CACHE_CAPACITY: usize = 512;

struct CacheEntry {
    parsed: Option<Rc<ParsedSelectorChain>>,
    last_used: u64,
}

#[derive(Default)]
struct SelectorParseCache {
    entries: HashMap<String, CacheEntry>,
    clock: u64,
    #[cfg(test)]
    parse_count: usize,
}

impl SelectorParseCache {
    fn get_or_parse(
        &mut self,
        selector: &str,
        parse: impl FnOnce(&str) -> Option<(Vec<CompoundSelector>, Vec<Combinator>)>,
    ) -> Option<Rc<ParsedSelectorChain>> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(selector) {
            entry.last_used = self.clock;
            return entry.parsed.clone();
        }

        #[cfg(test)]
        {
            self.parse_count += 1;
        }
        let parsed = parse(selector)
            .map(|(chain, combinators)| Rc::new(ParsedSelectorChain::new(chain, combinators)));
        if self.entries.len() >= SELECTOR_PARSE_CACHE_CAPACITY
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(selector, _)| selector.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            selector.to_string(),
            CacheEntry {
                parsed: parsed.clone(),
                last_used: self.clock,
            },
        );
        parsed
    }
}

thread_local! {
    static SELECTOR_PARSE_CACHE: RefCell<SelectorParseCache> =
        RefCell::new(SelectorParseCache::default());
}

pub(super) fn get_or_parse(
    selector: &str,
    parse: impl FnOnce(&str) -> Option<(Vec<CompoundSelector>, Vec<Combinator>)>,
) -> Option<Rc<ParsedSelectorChain>> {
    SELECTOR_PARSE_CACHE.with(|cache| cache.borrow_mut().get_or_parse(selector, parse))
}

#[cfg(test)]
pub(super) fn clear_for_test() {
    SELECTOR_PARSE_CACHE.with(|cache| *cache.borrow_mut() = SelectorParseCache::default());
}

#[cfg(test)]
pub(super) fn parse_count_for_test() -> usize {
    SELECTOR_PARSE_CACHE.with(|cache| cache.borrow().parse_count)
}

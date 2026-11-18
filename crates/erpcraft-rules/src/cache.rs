//! What was parsed before is not parsed again.
//!
//! Parsing costs a thread (see the `parse` module), and the same few
//! sources are parsed over and over: the permission rules of a role each
//! time a user is loaded, the rules of a policy each time it is asked.
//! The cache maps the exact source text to what parsing it gave.
//!
//! It is three caches, so that no kind of input can push out another:
//!
//! - **stored**: rules and expressions the installation keeps, such as
//!   permission rules and policies. Every request needs a handful of them.
//!   Only people who may change roles or policies can add to it.
//! - **sent**: conditions a caller sends, such as the `where` of a list.
//!   Any signed-in user can send any number of different ones; they
//!   evict each other and nothing else.
//! - **refused**: sources that could not be parsed, with the reason, so
//!   that the same broken condition sent again and again costs one
//!   parse. Different broken conditions evict each other and nothing else.
//!
//! Each is bounded twice, by entries and by the bytes of source text it
//! holds. A parsed tree was measured at 13 to 76 bytes of memory for each
//! byte of source (20 to 35 for conditions as people write them), so the
//! byte bound is what bounds memory: about 80 MB for `stored` if it were
//! filled to the last byte with the most expensive sources there are, 20
//! MB for `sent`, and under 1 MB for `refused`, which holds text only. An
//! installation with a hundred rules of a hundred characters uses about
//! a third of a megabyte.
//!
//! When a part of a cache is full, the entry used longest ago goes. A hit
//! takes a shared lock on one sixteenth of one cache and writes one
//! number; only a miss, which has just paid for a parse, locks that
//! sixteenth exclusively. Two callers that miss the same source at the
//! same moment both parse it; the results are equal and one replaces the
//! other.

use std::collections::HashMap;
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use cel::Program;

/// Where a source comes from, which decides whose room it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// Kept by the installation: a permission rule, a policy.
    Stored,
    /// Sent by a caller with a request.
    Sent,
}

/// How much one cache holds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bound {
    /// Independently locked parts. An entry lives in the part its source
    /// hashes to.
    pub shards: usize,
    /// Entries in each part.
    pub entries: usize,
    /// Bytes of text in each part: the sources, and for refusals the
    /// reasons too.
    pub bytes: usize,
}

/// The bounds of the three caches.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    pub stored: Bound,
    pub sent: Bound,
    pub refused: Bound,
}

impl Bounds {
    /// 1,024 stored sources of up to 1 MB together: a company with fifty
    /// roles of twenty conditional rules each fits, and a source is a
    /// hundred bytes far more often than the 4,000 it may be. 256 sent
    /// conditions of up to 256 KB together: the lists a busy interface
    /// keeps asking for, and no more than an attacker may fill. 256
    /// refusals: enough for every client that is retrying a broken
    /// request at one moment.
    pub(crate) const DEFAULT: Bounds = Bounds {
        stored: Bound {
            shards: 16,
            entries: 64,
            bytes: 64 * 1024,
        },
        sent: Bound {
            shards: 16,
            entries: 16,
            bytes: 16 * 1024,
        },
        refused: Bound {
            shards: 16,
            entries: 16,
            bytes: 16 * 1024,
        },
    };
}

struct Slot<V> {
    value: V,
    /// Bytes this entry counts for.
    bytes: usize,
    /// When it was last asked for, on the cache's clock.
    used: AtomicU64,
}

struct Shard<V> {
    slots: HashMap<Box<str>, Slot<V>>,
    bytes: usize,
}

/// One bounded cache from source text to `V`.
struct Tier<V> {
    shards: Box<[RwLock<Shard<V>>]>,
    bound: Bound,
    /// Which part a source goes to need not be secret: aiming sources at
    /// one part only makes them evict each other sooner.
    hasher: BuildHasherDefault<DefaultHasher>,
}

impl<V: Clone> Tier<V> {
    fn new(bound: Bound) -> Tier<V> {
        let shard = || {
            RwLock::new(Shard {
                slots: HashMap::new(),
                bytes: 0,
            })
        };
        Tier {
            shards: (0..bound.shards.max(1)).map(|_| shard()).collect(),
            bound,
            hasher: BuildHasherDefault::default(),
        }
    }

    fn shard(&self, source: &str) -> &RwLock<Shard<V>> {
        let index = self.hasher.hash_one(source) as usize % self.shards.len();
        &self.shards[index]
    }

    fn get(&self, source: &str, now: u64) -> Option<V> {
        // A panic while a lock was held leaves a map that is still a map.
        let shard = self
            .shard(source)
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let slot = shard.slots.get(source)?;
        slot.used.store(now, Ordering::Relaxed);
        Some(slot.value.clone())
    }

    /// Keep `value` for `source`, counting `extra` bytes besides the
    /// source. Whatever was used longest ago makes room. An entry too big
    /// for a part on its own is not kept.
    fn put(&self, source: &str, value: V, extra: usize, now: u64) {
        let bytes = source.len() + extra;
        if bytes > self.bound.bytes || self.bound.entries == 0 {
            return;
        }
        let mut shard = self
            .shard(source)
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = shard.slots.remove(source) {
            shard.bytes -= old.bytes;
        }
        while shard.slots.len() >= self.bound.entries || shard.bytes + bytes > self.bound.bytes {
            // A part holds a few dozen entries: looking at each is cheaper
            // than keeping them in order on every hit.
            let Some(oldest) = shard
                .slots
                .iter()
                .min_by_key(|(_, slot)| slot.used.load(Ordering::Relaxed))
                .map(|(source, _)| source.clone())
            else {
                break;
            };
            if let Some(old) = shard.slots.remove(&oldest) {
                shard.bytes -= old.bytes;
            }
        }
        shard.bytes += bytes;
        shard.slots.insert(
            source.into(),
            Slot {
                value,
                bytes,
                used: AtomicU64::new(now),
            },
        );
    }

    #[cfg(test)]
    /// Entries and bytes held, over all parts.
    fn size(&self) -> (usize, usize) {
        self.shards.iter().fold((0, 0), |(entries, bytes), shard| {
            let shard = shard.read().unwrap_or_else(PoisonError::into_inner);
            (entries + shard.slots.len(), bytes + shard.bytes)
        })
    }

    #[cfg(test)]
    fn limit(&self) -> (usize, usize) {
        (
            self.shards.len() * self.bound.entries,
            self.shards.len() * self.bound.bytes,
        )
    }
}

/// Why a source was not parsed.
pub(crate) struct Refusal {
    /// The reason, for the caller to put into its own message.
    pub why: String,
    /// Whether the same source would be refused the same way next time.
    /// Not so when the parser could not be started.
    pub lasting: bool,
}

pub(crate) struct Cache {
    stored: Tier<Arc<Program>>,
    sent: Tier<Arc<Program>>,
    refused: Tier<Arc<str>>,
    clock: AtomicU64,
    /// How often the parser was really run.
    parses: AtomicU64,
    enabled: bool,
}

#[cfg(test)]
/// What a cache holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Held {
    pub stored: (usize, usize),
    pub sent: (usize, usize),
    pub refused: (usize, usize),
    pub parses: u64,
}

impl Cache {
    pub(crate) fn new(bounds: Bounds) -> Cache {
        Cache {
            stored: Tier::new(bounds.stored),
            sent: Tier::new(bounds.sent),
            refused: Tier::new(bounds.refused),
            clock: AtomicU64::new(1),
            parses: AtomicU64::new(0),
            enabled: true,
        }
    }

    /// A cache that keeps nothing: every source is parsed every time.
    pub(crate) fn disabled() -> Cache {
        Cache {
            enabled: false,
            ..Cache::new(Bounds::DEFAULT)
        }
    }

    /// What parsing `source` gives, parsing it with `parse` only if that
    /// is not known yet.
    pub(crate) fn compile(
        &self,
        source: &str,
        origin: Origin,
        parse: impl FnOnce(&str) -> Result<Program, Refusal>,
    ) -> Result<Arc<Program>, String> {
        if !self.enabled {
            self.parses.fetch_add(1, Ordering::Relaxed);
            return parse(source).map(Arc::new).map_err(|refusal| refusal.why);
        }
        let now = self.clock.fetch_add(1, Ordering::Relaxed);
        if let Some(program) = self.stored.get(source, now) {
            return Ok(program);
        }
        if let Some(program) = self.sent.get(source, now) {
            // Something first seen in a request turns out to be stored:
            // from now on it is kept where requests cannot push it out.
            if origin == Origin::Stored {
                self.stored.put(source, program.clone(), 0, now);
            }
            return Ok(program);
        }
        if let Some(why) = self.refused.get(source, now) {
            return Err(why.to_string());
        }
        self.parses.fetch_add(1, Ordering::Relaxed);
        match parse(source) {
            Ok(program) => {
                let program = Arc::new(program);
                let tier = match origin {
                    Origin::Stored => &self.stored,
                    Origin::Sent => &self.sent,
                };
                tier.put(source, program.clone(), 0, now);
                Ok(program)
            }
            Err(refusal) => {
                if refusal.lasting {
                    self.refused
                        .put(source, refusal.why.as_str().into(), refusal.why.len(), now);
                }
                Err(refusal.why)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> Held {
        Held {
            stored: self.stored.size(),
            sent: self.sent.size(),
            refused: self.refused.size(),
            parses: self.parses.load(Ordering::Relaxed),
        }
    }

    #[cfg(test)]
    /// The most [`Cache::held`] can ever report, as entries and bytes.
    pub(crate) fn limits(&self) -> [(usize, usize); 3] {
        [self.stored.limit(), self.sent.limit(), self.refused.limit()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    const SMALL: Bounds = Bounds {
        stored: Bound {
            shards: 4,
            entries: 8,
            bytes: 2048,
        },
        sent: Bound {
            shards: 4,
            entries: 4,
            bytes: 512,
        },
        refused: Bound {
            shards: 2,
            entries: 4,
            bytes: 512,
        },
    };

    /// As the crate uses a cache, with the real parser behind it.
    fn compile(cache: &Cache, source: &str, origin: Origin) -> Result<Arc<Program>, String> {
        cache.compile(source, origin, parse::uncached)
    }

    /// What a result looks like from outside: the tree, or the reason.
    fn shown(result: &Result<Arc<Program>, String>) -> String {
        match result {
            Ok(program) => format!("{:?}", program.expression().expr),
            Err(why) => format!("refused: {why}"),
        }
    }

    fn within_limits(cache: &Cache) {
        let held = cache.held();
        for (held, limit) in [held.stored, held.sent, held.refused]
            .into_iter()
            .zip(cache.limits())
        {
            assert!(held.0 <= limit.0, "{held:?} entries of {limit:?}");
            assert!(held.1 <= limit.1, "{held:?} bytes of {limit:?}");
        }
    }

    #[test]
    fn a_cached_answer_is_the_answer_parsing_gives() {
        let cache = Cache::new(Bounds::DEFAULT);
        let nothing = Cache::disabled();
        let deep = vec!["1"; 100].join("+");
        let sources = [
            "record.warehouse in user.warehouses && (record.amount < 10000 || user.is_manager)",
            "facts.over_percent <= 5",
            "true",
            "record.a == 'x' ? 1 : [2, 3]",
            // Refused, each for another reason.
            "record.qty >",
            "(record.a == 1",
            "record.a = 1",
            deep.as_str(),
        ];
        for origin in [Origin::Sent, Origin::Stored] {
            for source in sources {
                let fresh = shown(&compile(&nothing, source, origin));
                // The first answer is parsed, the next two are remembered.
                for _ in 0..3 {
                    assert_eq!(shown(&compile(&cache, source, origin)), fresh, "{source}");
                }
            }
        }
        // Each source was parsed once, whatever it was asked as afterwards.
        assert_eq!(cache.held().parses, sources.len() as u64);
        assert_eq!(nothing.held().parses, 2 * sources.len() as u64);
        assert_eq!(nothing.held().stored, (0, 0));
        // The very same tree is handed out, not a copy.
        let first = compile(&cache, "true", Origin::Sent).unwrap();
        let again = compile(&cache, "true", Origin::Stored).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
    }

    #[test]
    fn what_could_not_be_tried_is_not_remembered_as_refused() {
        let cache = Cache::new(SMALL);
        let unlucky = |_: &str| {
            Err(Refusal {
                why: "it could not be parsed: no thread".into(),
                lasting: false,
            })
        };
        assert!(cache.compile("true", Origin::Sent, unlucky).is_err());
        assert_eq!(cache.held().refused, (0, 0));
        assert!(compile(&cache, "true", Origin::Sent).is_ok());
    }

    #[test]
    fn a_flood_of_sources_stays_within_bounds_and_spares_what_is_stored() {
        let cache = Cache::new(SMALL);
        // What every request needs.
        let stored: Vec<String> = (0..12)
            .map(|n| format!("record.warehouse == 'W{n}'"))
            .collect();
        for source in &stored {
            compile(&cache, source, Origin::Stored).unwrap();
        }
        let before = cache.held();
        assert_eq!(before.stored.0, stored.len());

        // Endless different conditions from callers, good and broken,
        // short and as long as a source may be.
        for n in 0..400 {
            let padding = " ".repeat(n % 7 * 60);
            compile(&cache, &format!("record.qty{padding} == {n}"), Origin::Sent).unwrap();
            compile(
                &cache,
                &format!("record.qty{padding} == {n} &&"),
                Origin::Sent,
            )
            .unwrap_err();
            within_limits(&cache);
        }
        let long = format!("record.code in [{}]", vec!["'ABCDEFGH'"; 300].join(", "));
        assert!(long.len() > SMALL.sent.bytes && long.len() <= parse::MAX_SOURCE_LEN);
        compile(&cache, &long, Origin::Sent).unwrap();
        within_limits(&cache);
        let after = cache.held();
        // Both hold what fits, by entries or by bytes, and no more.
        assert!((4..=16).contains(&after.sent.0), "{after:?}");
        assert!((2..=8).contains(&after.refused.0), "{after:?}");

        // Nothing stored was pushed out: asking again parses nothing.
        assert_eq!(after.stored, before.stored);
        for source in &stored {
            compile(&cache, source, Origin::Stored).unwrap();
        }
        assert_eq!(cache.held().parses, after.parses);

        // The stored cache is bounded too, and drops what was used
        // longest ago: the twelve are asked for all along and stay.
        for n in 0..200 {
            compile(&cache, &format!("record.owner == 'U{n}'"), Origin::Stored).unwrap();
            for source in &stored {
                compile(&cache, source, Origin::Stored).unwrap();
            }
            within_limits(&cache);
        }
        let parses = cache.held().parses;
        for source in &stored {
            compile(&cache, source, Origin::Stored).unwrap();
        }
        // Twelve entries in four parts of eight: a part may get more than
        // eight of the 212, never more than eight at once.
        assert!(cache.held().parses - parses <= 4, "the ones in use stay");
        assert!(cache.held().stored.0 <= 32);
    }

    #[test]
    fn many_threads_get_the_right_answers() {
        // Small enough that entries are evicted all the time.
        let cache = Cache::new(SMALL);
        let sources: Vec<String> = (0..40)
            .map(|n| match n % 4 {
                0 => format!("record.qty == {n}"),
                1 => format!("record.qty == {n} ||"),
                2 => format!("user.limit > {n} && record.active"),
                _ => format!("(record.qty == {n}"),
            })
            .collect();
        let nothing = Cache::disabled();
        let expected: Vec<String> = sources
            .iter()
            .map(|source| shown(&compile(&nothing, source, Origin::Sent)))
            .collect();

        std::thread::scope(|scope| {
            for thread in 0..16 {
                let (cache, sources, expected) = (&cache, &sources, &expected);
                scope.spawn(move || {
                    for round in 0..60 {
                        let index = (thread * 7 + round * 13) % sources.len();
                        let origin = if (thread + round) % 3 == 0 {
                            Origin::Stored
                        } else {
                            Origin::Sent
                        };
                        let got = shown(&compile(cache, &sources[index], origin));
                        assert_eq!(got, expected[index]);
                    }
                });
            }
        });
        within_limits(&cache);
        // Far fewer parses than questions.
        assert!(cache.held().parses < 16 * 60);
    }

    #[test]
    fn the_default_bounds_are_what_the_documentation_says() {
        let cache = Cache::new(Bounds::DEFAULT);
        assert_eq!(
            cache.limits(),
            [(1024, 1024 * 1024), (256, 256 * 1024), (256, 256 * 1024)]
        );
    }
}

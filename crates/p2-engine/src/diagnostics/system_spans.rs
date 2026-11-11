use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use bevy::ecs::prelude::Resource;
use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::span::{Attributes, Id};
use bevy::log::tracing::Subscriber;
use bevy::log::tracing_subscriber::Layer;
use bevy::log::tracing_subscriber::layer::Context;
use bevy::log::tracing_subscriber::registry::LookupSpan;
use rustc_hash::FxHashMap;

/// Time per Bevy system, summed over all threads, and its longest single run (what a hitch is made of). Bevy opens
/// these spans only in a build with its `trace` feature.
#[derive(Resource, Clone, Default)]
pub struct SystemSpans(Arc<Mutex<FxHashMap<Arc<str>, [f64; 2]>>>);

impl SystemSpans {
    /// The `count` systems with the most time, per frame, and the `longest` single runs.
    pub(super) fn take_slowest(&self, count: usize, longest: usize, frames: u32) -> [Vec<(Arc<str>, f64)>; 2] {
        let mut spent = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let all: Vec<_> = spent.drain().collect();
        let frames = f64::from(frames.max(1));
        [(0, count, frames), (1, longest, 1.0)].map(|(which, count, over)| {
            let mut slowest: Vec<(Arc<str>, f64)> = all.iter().map(|(name, ms)| (name.clone(), ms[which])).collect();
            slowest.sort_by(|a, b| b.1.total_cmp(&a.1));
            slowest.truncate(count);
            slowest.into_iter().map(|(name, ms)| (name, ms / over)).collect()
        })
    }
}

struct SpanClock {
    name: Arc<str>,
    entered: Option<Instant>,
}

#[derive(Default)]
struct SpanName(String);

impl Visit for SpanName {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "name" {
            value.clone_into(&mut self.0);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "name" {
            self.0 = format!("{value:?}");
        }
    }
}

pub(super) struct SpanTiming(pub(super) SystemSpans);

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for SpanTiming {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let prefix = match attrs.metadata().name() {
            "system" => "",
            "system_commands" => "commands of ",
            _ => return,
        };
        let mut name = SpanName::default();
        attrs.record(&mut name);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanClock { name: format!("{prefix}{}", name.0).into(), entered: None });
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(clock) = span.extensions_mut().get_mut::<SpanClock>()
        {
            clock.entered = Some(Instant::now());
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        let Some(clock) = extensions.get_mut::<SpanClock>() else { return };
        let Some(entered) = clock.entered.take() else { return };
        let ms = entered.elapsed().as_secs_f64() * 1000.0;
        let mut spent = self.0.0.lock().unwrap_or_else(PoisonError::into_inner);
        let [total, longest] = spent.entry(clock.name.clone()).or_default();
        *total += ms;
        *longest = longest.max(ms);
    }
}

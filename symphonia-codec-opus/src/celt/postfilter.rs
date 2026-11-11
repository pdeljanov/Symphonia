//! CELT pitch comb post-filter
//! ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
//!
//! After the inverse MDCT, each frame is filtered by a 5-tap comb at the decoded pitch period
//! with a decoded gain and tapset; when the setting changes between frames the first `OVERLAP`
//! samples cross-fade from the old filter output to the new one with the squared MDCT window.
//!
//! ```text
//!   y[i] = x[i] + G * ( g0 * x[i - T]
//!                      + g1 * (x[i - T - 1] + x[i - T + 1])
//!                      + g2 * (x[i - T - 2] + x[i - T + 2]) )
//!   cross-fade:  y[i] = x[i] + (1 - w[i]^2) * comb_old[i] + w[i]^2 * comb_new[i]
//! ```

use crate::celt::constant::{CELT_WINDOW, COMB_FILTER_GAINS};
use crate::celt::rate::PostFilterParams;
use crate::celt::{BlockSize, DECODE_BUFFER_SIZE, MIN_PITCH_PERIOD, OVERLAP, SHORT_MDCT_SIZE};

/// One channel of synthesis history: `DECODE_BUFFER_SIZE` past samples followed by the `OVERLAP`
/// samples of the pending MDCT overlap-add tail
/// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
pub(crate) type ChannelHistory = [f32; DECODE_BUFFER_SIZE + OVERLAP];

/// Comb post-filter state carried across frames: the setting of the most recent frame and the
/// setting it cross-faded from over its first `OVERLAP` samples
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CombFilter {
    /// Pitch period, gain and tapset in force at the end of the most recent frame.
    pub(crate) current: PostFilterParams,
    /// Pitch period, gain and tapset the most recent frame cross-faded from.
    pub(crate) previous: PostFilterParams,
}

impl CombFilter {
    /// Returns the filter to its initial disabled state (zero period, gain and tapset)
    /// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
        return;
    }

    /// The current setting with its period raised to the minimum comb period, which a lost
    /// frame keeps applying
    /// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
    pub(crate) fn held(&self) -> PostFilterParams {
        return PostFilterParams {
            period: self.current.period.max(MIN_PITCH_PERIOD),
            gain: self.current.gain,
            tapset: self.current.tapset,
        };
    }

    /// Filters the `n` newest samples of every channel in `history` in place and advances the
    /// state to `next`: the first short block cross-fades from `previous` to `current`, the rest
    /// of a long frame from `current` to `next`
    /// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
    pub(crate) fn apply(
        &mut self,
        history: &mut [ChannelHistory],
        n: usize,
        lm: BlockSize,
        next: PostFilterParams,
    ) {
        let out_offset = DECODE_BUFFER_SIZE - n;
        self.current.period = self.current.period.max(MIN_PITCH_PERIOD);
        self.previous.period = self.previous.period.max(MIN_PITCH_PERIOD);

        for channel in history.iter_mut() {
            filter_in_place(channel, out_offset, self.previous, self.current, SHORT_MDCT_SIZE);
            if lm.log2() != 0 {
                filter_in_place(
                    channel,
                    out_offset + SHORT_MDCT_SIZE,
                    self.current,
                    next,
                    n - SHORT_MDCT_SIZE,
                );
            }
        }

        self.previous = self.current;
        self.current = next;
        if lm.log2() != 0 {
            self.previous = self.current;
        }
        return;
    }

    /// Undoes the comb filter on the `OVERLAP` samples that follow a pitch-based concealment and
    /// folds them with the MDCT window, so the next decoded frame overlap-adds onto them
    /// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>),
    /// [Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn prefilter_and_fold(&self, history: &mut [ChannelHistory], n: usize) {
        let history_offset = DECODE_BUFFER_SIZE - n;
        let mut etmp = [0.0_f32; OVERLAP];

        for channel in history.iter_mut() {
            comb_filter(
                &mut etmp,
                channel,
                history_offset,
                inverted(self.previous),
                inverted(self.current),
            );

            let half_overlap = OVERLAP / 2;
            for i in 0..half_overlap {
                channel[history_offset + i] =
                    CELT_WINDOW[i] * etmp[OVERLAP - 1 - i] + CELT_WINDOW[OVERLAP - 1 - i] * etmp[i];
            }
        }
        return;
    }
}

/// The comb filter setting with its gain negated, which undoes a filter already applied
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
#[inline]
fn inverted(params: PostFilterParams) -> PostFilterParams {
    return PostFilterParams { gain: -params.gain, ..params };
}

/// Tap weights of one comb filter setting: the gain times the tapset's three symmetric taps
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
#[derive(Debug, Clone, Copy, PartialEq)]
struct CombFilterTaps {
    /// Weight of `x[i - T]`.
    center: f32,
    /// Weight of `x[i - T - 1]` and `x[i - T + 1]`.
    first: f32,
    /// Weight of `x[i - T - 2]` and `x[i - T + 2]`.
    second: f32,
}

impl CombFilterTaps {
    /// Tap weights of `params`, scaled by `weight` (the cross-fade factor, or `1`).
    #[inline]
    fn of(params: PostFilterParams, weight: f32) -> Self {
        let [center, first, second] =
            COMB_FILTER_GAINS[params.tapset.min(2)].map(|tap| (weight * params.gain) * tap);
        return Self { center, first, second };
    }

    /// Comb term of the taps centred on `x[p]`.
    #[inline]
    fn comb(&self, x: &[f32], p: usize) -> f32 {
        return self.center * x[p]
            + self.first * (x[p + 1] + x[p - 1])
            + self.second * (x[p + 2] + x[p - 2]);
    }
}

/// Pitch period of a comb filter setting, raised to [`MIN_PITCH_PERIOD`].
#[inline]
fn period_of(params: PostFilterParams) -> usize {
    return params.period.max(MIN_PITCH_PERIOD);
}

/// Applies the 5-tap comb post-filter recursively in place on
/// `signal_buf[history_offset..history_offset + n]`, cross-fading from `from` to `to` over the
/// first `OVERLAP` samples with the squared MDCT window
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
fn filter_in_place(
    signal_buf: &mut [f32],
    history_offset: usize,
    from: PostFilterParams,
    to: PostFilterParams,
    n: usize,
) {
    let (g0, g1) = (from.gain, to.gain);
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }

    let period0 = period_of(from);
    let period1 = period_of(to);
    debug_assert!(history_offset >= period0 + 2);
    debug_assert!(history_offset >= period1 + 2);

    let old = CombFilterTaps::of(from, 1.0);
    let new = CombFilterTaps::of(to, 1.0);

    let mut x1 = signal_buf[history_offset - period1 + 1];
    let mut x2 = signal_buf[history_offset - period1];
    let mut x3 = signal_buf[history_offset - period1 - 1];
    let mut x4 = signal_buf[history_offset - period1 - 2];

    let same_filter = g0 == g1 && period0 == period1 && from.tapset == to.tapset;
    let xfade_len = if same_filter { 0 } else { OVERLAP.min(n) };

    for (i, &w) in CELT_WINDOW.iter().enumerate().take(xfade_len) {
        let pos = history_offset + i;
        let x0 = signal_buf[pos - period1 + 2];
        let f = w * w;
        let fading_out = CombFilterTaps::of(from, 1.0_f32 - f);
        let fading_in = CombFilterTaps::of(to, f);

        signal_buf[pos] = signal_buf[pos]
            + fading_out.comb(signal_buf, pos - period0)
            + fading_in.center * x2
            + fading_in.first * (x1 + x3)
            + fading_in.second * (x0 + x4);

        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }

    if g1 == 0.0 {
        return;
    }

    for i in xfade_len..n {
        let pos = history_offset + i;
        let x0 = signal_buf[pos - period1 + 2];
        signal_buf[pos] =
            signal_buf[pos] + new.center * x2 + new.first * (x1 + x3) + new.second * (x0 + x4);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
    let _ = old;
    return;
}

/// Applies the 5-tap symmetric pitch comb post-filter, reading from `x_buf` at `history_offset`
/// and writing `y_out.len()` filtered samples into `y_out`
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
///
/// When `from` and `to` differ, the first `OVERLAP` samples cross-fade from the old filter
/// output to the new filter output with the squared MDCT window weight `w[i]^2`.
pub(crate) fn comb_filter(
    y_out: &mut [f32],
    x_buf: &[f32],
    history_offset: usize,
    from: PostFilterParams,
    to: PostFilterParams,
) {
    let n = y_out.len();
    debug_assert!(x_buf.len() >= history_offset + n);

    let (g0, g1) = (from.gain, to.gain);
    if g0 == 0.0 && g1 == 0.0 {
        y_out.copy_from_slice(&x_buf[history_offset..history_offset + n]);
        return;
    }

    let period0 = period_of(from);
    let period1 = period_of(to);
    debug_assert!(history_offset >= period0 + 2);
    debug_assert!(history_offset >= period1 + 2);

    let old = CombFilterTaps::of(from, 1.0);
    let new = CombFilterTaps::of(to, 1.0);

    let same_filter = g0 == g1 && period0 == period1 && from.tapset == to.tapset;
    let xfade_len = if same_filter { 0 } else { OVERLAP.min(n) };

    for (i, (y_slot, &w)) in y_out.iter_mut().zip(CELT_WINDOW.iter()).enumerate().take(xfade_len) {
        let pos = history_offset + i;
        let f = w * w;
        let old_comb = old.comb(x_buf, pos - period0);
        let new_comb = new.comb(x_buf, pos - period1);
        *y_slot = x_buf[pos] + (1.0 - f) * old_comb + f * new_comb;
    }

    if g1 == 0.0 {
        y_out[xfade_len..].copy_from_slice(&x_buf[history_offset + xfade_len..history_offset + n]);
        return;
    }

    for (i, y_slot) in y_out.iter_mut().enumerate().skip(xfade_len) {
        let pos = history_offset + i;
        *y_slot = x_buf[pos] + new.comb(x_buf, pos - period1);
    }
    return;
}

#[cfg(test)]
mod tests;

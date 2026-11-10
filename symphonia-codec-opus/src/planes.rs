//! Planar `f32` channel pairs handed down the decode chain
//! ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>)).
//!
//! Every Opus stream decodes into one (mono) or two (coupled stereo) planar `f32` sample planes.
//! The SILK layer, the CELT layer, the Hybrid summation and the redundancy cross-fades of mode
//! transitions ([Section 4.5.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1>))
//! all write into the same pair of planes. [`Planes`] bundles both channels so that a nested
//! decode stage receives the same sub-range of each plane without consuming the parent borrow.
//!
//! ```text
//!             +------------------------------+
//!   left  --->| sample 0 | sample 1 | ...    |  always present
//!             +------------------------------+
//!   right --->| sample 0 | sample 1 | ...    |  present for coupled stereo only
//!             +------------------------------+
//! ```

use core::slice::SliceIndex;

/// One or two planar `f32` output channels of equal length
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
///
/// `left` is always present; `right` is `Some` only for a coupled stereo stream.
pub(crate) struct Planes<'a> {
    /// The first channel: mono, or left for stereo.
    pub(crate) left: &'a mut [f32],
    /// The second channel, right, present for coupled stereo only.
    pub(crate) right: Option<&'a mut [f32]>,
}

impl<'a> Planes<'a> {
    /// Wraps `left` and an optional `right` plane.
    #[inline]
    pub(crate) fn new(left: &'a mut [f32], right: Option<&'a mut [f32]>) -> Self {
        return Self { left, right };
    }

    /// Wraps a single mono plane.
    #[cfg(test)]
    #[inline]
    pub(crate) fn mono(left: &'a mut [f32]) -> Self {
        return Self::new(left, None);
    }

    /// Views a two-row scratch array as planes, exposing the second row only when `stereo`.
    #[inline]
    pub(crate) fn from_rows<const N: usize>(rows: &'a mut [[f32; N]; 2], stereo: bool) -> Self {
        let [left, right] = rows;
        return Self { left, right: stereo.then_some(right.as_mut_slice()) };
    }

    /// Returns `true` when a right plane is present.
    #[inline]
    pub(crate) fn is_stereo(&self) -> bool {
        return self.right.is_some();
    }

    /// Reborrows both planes for a nested decode stage without consuming `self`.
    #[inline]
    pub(crate) fn reborrow(&mut self) -> Planes<'_> {
        return Planes::new(&mut *self.left, self.right.as_deref_mut());
    }

    /// Reborrows the same `range` (`..n`, `a..b`, or `a..`) of both planes.
    #[inline]
    pub(crate) fn range<R>(&mut self, range: R) -> Planes<'_>
    where
        R: SliceIndex<[f32], Output = [f32]> + Clone,
    {
        return Planes::new(
            &mut self.left[range.clone()],
            self.right.as_deref_mut().map(|right| &mut right[range]),
        );
    }

    /// Fills every sample of both planes with `value`.
    #[inline]
    pub(crate) fn fill(&mut self, value: f32) {
        self.each_mut().for_each(|plane| plane.fill(value));
        return;
    }

    /// Iterates the left plane, then the right plane when present.
    #[inline]
    pub(crate) fn each_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        return core::iter::once(&mut *self.left).chain(self.right.as_deref_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_rows_exposes_right_row_only_for_stereo() {
        let mut rows = [[0.0_f32; 4]; 2];

        let mono = Planes::from_rows(&mut rows, false);
        assert!(!mono.is_stereo());
        assert_eq!(mono.left.len(), 4);

        let stereo = Planes::from_rows(&mut rows, true);
        assert!(stereo.is_stereo());
        assert_eq!(stereo.right.as_deref().map(<[f32]>::len), Some(4));
        return;
    }

    #[test]
    fn range_selects_the_same_window_of_both_planes() {
        let mut rows = [[0.0_f32; 6]; 2];
        let mut planes = Planes::from_rows(&mut rows, true);

        planes.range(2..4).fill(1.0);
        planes.range(5..).fill(2.0);

        let expected = [0.0, 0.0, 1.0, 1.0, 0.0, 2.0];
        assert_eq!(rows[0], expected);
        assert_eq!(rows[1], expected);
        return;
    }

    #[test]
    fn fill_and_each_mut_cover_only_present_planes() {
        let mut left = [0.0_f32; 3];
        let mut right = [0.0_f32; 3];

        let mut mono = Planes::mono(&mut left);
        assert_eq!(mono.each_mut().count(), 1);
        mono.fill(0.5);
        assert_eq!(left, [0.5; 3]);
        assert_eq!(right, [0.0; 3]);

        let mut stereo = Planes::new(&mut left, Some(&mut right));
        assert_eq!(stereo.each_mut().count(), 2);
        stereo.range(..2).fill(-1.0);
        assert_eq!(left, [-1.0, -1.0, 0.5]);
        assert_eq!(right, [-1.0, -1.0, 0.0]);
        return;
    }

    #[test]
    fn reborrow_keeps_the_parent_usable() {
        let mut left = [1.0_f32; 2];
        let mut right = [1.0_f32; 2];
        let mut planes = Planes::new(&mut left, Some(&mut right));

        planes.reborrow().fill(0.0);
        assert!(planes.is_stereo());
        assert_eq!(planes.left.len(), 2);
        assert_eq!(left, [0.0; 2]);
        assert_eq!(right, [0.0; 2]);
        return;
    }
}

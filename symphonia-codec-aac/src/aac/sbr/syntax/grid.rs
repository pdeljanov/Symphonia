// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The SBR time/frequency grid: sbr_grid() (ISO/IEC 14496-3:2009, Table 4.69) and the calculation
//! of the envelope and noise floor time borders (4.6.18.3.3).

use symphonia_core::errors::{Result, decode_error};

use crate::aac::sbr::types::{MAX_ENVELOPES, MAX_NOISE_FLOORS, NUM_TIME_SLOTS};

use super::SbrBitReader;

/// The maximum number of envelopes of an SBR frame of class FIXFIX (4.6.18.3.6).
const MAX_FIXFIX_ENVELOPES: usize = 4;

/// bs_frame_class (Table 4.114).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameClass {
    #[default]
    FixFix,
    FixVar,
    VarFix,
    VarVar,
}

/// The data elements of sbr_grid() for one channel.
#[derive(Clone, Copy, Debug, Default)]
pub struct GridFields {
    /// bs_frame_class.
    pub frame_class: FrameClass,
    /// bs_num_env.
    pub num_env: usize,
    /// bs_var_bord_0.
    pub var_bord_0: usize,
    /// bs_var_bord_1.
    pub var_bord_1: usize,
    /// bs_num_rel_0.
    pub num_rel_0: usize,
    /// bs_num_rel_1.
    pub num_rel_1: usize,
    /// bs_rel_bord_0.
    pub rel_bord_0: [usize; 3],
    /// bs_rel_bord_1.
    pub rel_bord_1: [usize; 3],
    /// bs_pointer.
    pub pointer: usize,
    /// bs_freq_res.
    pub freq_res: [bool; MAX_ENVELOPES],
}

/// The time/frequency grid of one channel of an SBR frame (4.6.18.3.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SbrGrid {
    /// L_E, the number of envelopes.
    pub num_env: usize,
    /// t_E, the envelope time borders, L_E + 1 entries are valid.
    pub t_env: [usize; MAX_ENVELOPES + 1],
    /// r(l), the frequency resolution of each envelope (true = high), L_E entries are valid.
    pub freq_res: [bool; MAX_ENVELOPES],
    /// L_Q, the number of noise floors.
    pub num_noise: usize,
    /// t_Q, the noise floor time borders, L_Q + 1 entries are valid.
    pub t_noise: [usize; MAX_NOISE_FLOORS + 1],
    /// l_A, or None if l_A = -1 (Table 4.176).
    pub l_a: Option<usize>,
}

/// The number of bits of bs_pointer, ptr_bits = ceil(log(bs_num_env + 1) / log(2)).
fn pointer_bits(num_env: usize) -> u32 {
    usize::BITS - num_env.leading_zeros()
}

/// Read a relative border length, bs_rel_bord = 2 * tmp + 2.
fn read_rel_bord(bs: &mut SbrBitReader<'_>) -> Result<usize> {
    Ok(2 * bs.read(2)? as usize + 2)
}

/// Read sbr_grid() (Table 4.69) and calculate the time/frequency grid.
///
/// `bs_amp_res` is set to false if the frame class is FIXFIX with a single envelope.
pub fn read_grid(bs: &mut SbrBitReader<'_>, bs_amp_res: &mut bool) -> Result<SbrGrid> {
    let mut f = GridFields {
        frame_class: match bs.read(2)? {
            0 => FrameClass::FixFix,
            1 => FrameClass::FixVar,
            2 => FrameClass::VarFix,
            _ => FrameClass::VarVar,
        },
        ..Default::default()
    };

    match f.frame_class {
        FrameClass::FixFix => {
            f.num_env = 1 << bs.read(2)?;

            if f.num_env == 1 {
                *bs_amp_res = false;
            }

            let freq_res = bs.read_bool()?;

            // L_E <= 4 for FIXFIX (4.6.18.3.6).
            if f.num_env > MAX_FIXFIX_ENVELOPES {
                return decode_error("sbr: too many envelopes");
            }

            f.freq_res[..f.num_env].fill(freq_res);
        }
        FrameClass::FixVar => {
            f.var_bord_1 = bs.read(2)? as usize;
            f.num_rel_1 = bs.read(2)? as usize;
            f.num_env = f.num_rel_1 + 1;

            for rel in 0..f.num_rel_1 {
                f.rel_bord_1[rel] = read_rel_bord(bs)?;
            }

            f.pointer = bs.read(pointer_bits(f.num_env))? as usize;

            for env in 0..f.num_env {
                f.freq_res[f.num_env - 1 - env] = bs.read_bool()?;
            }
        }
        FrameClass::VarFix => {
            f.var_bord_0 = bs.read(2)? as usize;
            f.num_rel_0 = bs.read(2)? as usize;
            f.num_env = f.num_rel_0 + 1;

            for rel in 0..f.num_rel_0 {
                f.rel_bord_0[rel] = read_rel_bord(bs)?;
            }

            f.pointer = bs.read(pointer_bits(f.num_env))? as usize;

            for env in 0..f.num_env {
                f.freq_res[env] = bs.read_bool()?;
            }
        }
        FrameClass::VarVar => {
            f.var_bord_0 = bs.read(2)? as usize;
            f.var_bord_1 = bs.read(2)? as usize;
            f.num_rel_0 = bs.read(2)? as usize;
            f.num_rel_1 = bs.read(2)? as usize;
            f.num_env = f.num_rel_0 + f.num_rel_1 + 1;

            // L_E <= 5 for VARVAR (4.6.18.3.6).
            if f.num_env > MAX_ENVELOPES {
                return decode_error("sbr: too many envelopes");
            }

            for rel in 0..f.num_rel_0 {
                f.rel_bord_0[rel] = read_rel_bord(bs)?;
            }

            for rel in 0..f.num_rel_1 {
                f.rel_bord_1[rel] = read_rel_bord(bs)?;
            }

            f.pointer = bs.read(pointer_bits(f.num_env))? as usize;

            for env in 0..f.num_env {
                f.freq_res[env] = bs.read_bool()?;
            }
        }
    }

    compute_grid(&f)
}

/// Calculate the time/frequency grid from the data elements of sbr_grid() (4.6.18.3.3).
///
/// Verifies that the number of envelopes is within the limits of 4.6.18.3.6, that bs_pointer
/// points to a valid border, and that the envelope and noise floor time borders are strictly
/// increasing.
pub fn compute_grid(f: &GridFields) -> Result<SbrGrid> {
    let num_env = f.num_env;

    let max_env = match f.frame_class {
        FrameClass::FixFix => MAX_FIXFIX_ENVELOPES,
        _ => MAX_ENVELOPES,
    };

    if num_env == 0 || num_env > max_env {
        return decode_error("sbr: invalid number of envelopes");
    }

    let num_rel_lead = match f.frame_class {
        FrameClass::FixFix => num_env - 1,
        FrameClass::FixVar => 0,
        FrameClass::VarFix | FrameClass::VarVar => f.num_rel_0,
    };

    let num_rel_trail = match f.frame_class {
        FrameClass::FixFix | FrameClass::VarFix => 0,
        FrameClass::FixVar | FrameClass::VarVar => f.num_rel_1,
    };

    // At most 3 relative borders can be signalled in each direction.
    if num_rel_lead > 3 || num_rel_trail > 3 || num_rel_lead + num_rel_trail + 1 != num_env {
        return decode_error("sbr: inconsistent number of relative borders");
    }

    // absBordLead and absBordTrail.
    let abs_bord_lead = match f.frame_class {
        FrameClass::FixFix | FrameClass::FixVar => 0,
        FrameClass::VarFix | FrameClass::VarVar => f.var_bord_0,
    };

    let abs_bord_trail = match f.frame_class {
        FrameClass::FixFix | FrameClass::VarFix => NUM_TIME_SLOTS,
        FrameClass::FixVar | FrameClass::VarVar => f.var_bord_1 + NUM_TIME_SLOTS,
    };

    // relBordLead(l): NINT(numTimeSlots / L_E) for FIXFIX, otherwise bs_rel_bord_0(l).
    let rel_bord_lead = |l: usize| -> i64 {
        match f.frame_class {
            FrameClass::FixFix => ((2 * NUM_TIME_SLOTS + num_env) / (2 * num_env)) as i64,
            _ => f.rel_bord_0[l] as i64,
        }
    };

    // t_E(l), computed with signed arithmetic, and verified to be strictly increasing below.
    let mut t_env = [0i64; MAX_ENVELOPES + 1];

    t_env[0] = abs_bord_lead as i64;
    t_env[num_env] = abs_bord_trail as i64;

    for l in 1..=num_rel_lead {
        t_env[l] = t_env[l - 1] + rel_bord_lead(l - 1);
    }

    for l in num_rel_lead + 1..num_env {
        let sum: i64 = f.rel_bord_1[..num_env - l].iter().map(|&b| b as i64).sum();
        t_env[l] = abs_bord_trail as i64 - sum;
    }

    if t_env[0] < 0 || t_env[..=num_env].windows(2).any(|w| w[0] >= w[1]) {
        return decode_error("sbr: envelope time borders are not strictly increasing");
    }

    let mut grid = SbrGrid { num_env, freq_res: f.freq_res, ..Default::default() };

    for (t, &b) in grid.t_env.iter_mut().zip(t_env.iter()).take(num_env + 1) {
        *t = b as usize;
    }

    // bs_pointer must point to a border within the frame (Tables 4.174 and 4.176).
    if f.frame_class != FrameClass::FixFix && f.pointer > num_env {
        return decode_error("sbr: invalid bs_pointer");
    }

    // l_A (Table 4.176).
    grid.l_a = match f.frame_class {
        FrameClass::FixFix => None,
        FrameClass::FixVar | FrameClass::VarVar => match f.pointer {
            0 => None,
            p => Some(num_env + 1 - p),
        },
        FrameClass::VarFix => match f.pointer {
            0 | 1 => None,
            p => Some(p - 1),
        },
    };

    // L_Q and t_Q.
    if num_env == 1 {
        grid.num_noise = 1;
        grid.t_noise[0] = grid.t_env[0];
        grid.t_noise[1] = grid.t_env[1];
    }
    else {
        // middleBorder (Table 4.174).
        let middle_border = match f.frame_class {
            FrameClass::FixFix => num_env / 2,
            FrameClass::VarFix => match f.pointer {
                0 => 1,
                1 => num_env - 1,
                p => p - 1,
            },
            FrameClass::FixVar | FrameClass::VarVar => match f.pointer {
                0 | 1 => num_env - 1,
                p => num_env + 1 - p,
            },
        };

        if middle_border == 0 || middle_border >= num_env {
            return decode_error("sbr: invalid noise floor time border");
        }

        grid.num_noise = 2;
        grid.t_noise[0] = grid.t_env[0];
        grid.t_noise[1] = grid.t_env[middle_border];
        grid.t_noise[2] = grid.t_env[num_env];
    }

    Ok(grid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(class: FrameClass, num_env: usize) -> GridFields {
        GridFields { frame_class: class, num_env, ..Default::default() }
    }

    #[test]
    fn verify_pointer_bits() {
        assert_eq!(pointer_bits(1), 1);
        assert_eq!(pointer_bits(2), 2);
        assert_eq!(pointer_bits(3), 2);
        assert_eq!(pointer_bits(4), 3);
        assert_eq!(pointer_bits(5), 3);
    }

    #[test]
    fn verify_fixfix() {
        // L_E = 1: t_E = [0, 16], t_Q = [0, 16].
        let g = compute_grid(&fields(FrameClass::FixFix, 1)).unwrap();
        assert_eq!((g.num_env, &g.t_env[..2]), (1, &[0, 16][..]));
        assert_eq!((g.num_noise, &g.t_noise[..2]), (1, &[0, 16][..]));
        assert_eq!(g.l_a, None);

        // L_E = 2: relBordLead = NINT(16 / 2) = 8.
        let g = compute_grid(&fields(FrameClass::FixFix, 2)).unwrap();
        assert_eq!(g.t_env[..3], [0, 8, 16]);
        assert_eq!((g.num_noise, &g.t_noise[..3]), (2, &[0, 8, 16][..]));

        // L_E = 4: relBordLead = 4, middleBorder = L_E / 2 = 2.
        let g = compute_grid(&fields(FrameClass::FixFix, 4)).unwrap();
        assert_eq!(g.t_env[..5], [0, 4, 8, 12, 16]);
        assert_eq!(g.t_noise[..3], [0, 8, 16]);
        assert_eq!(g.l_a, None);

        // L_E <= 4 for FIXFIX.
        assert!(compute_grid(&fields(FrameClass::FixFix, 8)).is_err());
        assert!(compute_grid(&fields(FrameClass::FixFix, 5)).is_err());
        assert!(compute_grid(&fields(FrameClass::FixFix, 0)).is_err());
    }

    #[test]
    fn verify_fixvar() {
        // absBordLead = 0, absBordTrail = 2 + 16 = 18, n_RelLead = 0, n_RelTrail = 2.
        // t_E(1) = 18 - (4 + 6) = 8, t_E(2) = 18 - 4 = 14.
        let mut f = fields(FrameClass::FixVar, 3);
        f.var_bord_1 = 2;
        f.num_rel_1 = 2;
        f.rel_bord_1 = [4, 6, 0];
        f.pointer = 2;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_env[..4], [0, 8, 14, 18]);
        // middleBorder = L_E + 1 - bs_pointer = 2, l_A = 2.
        assert_eq!(g.t_noise[..3], [0, 14, 18]);
        assert_eq!(g.l_a, Some(2));

        // bs_pointer = 0: middleBorder = L_E - 1 = 2, l_A = -1.
        f.pointer = 0;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_noise[..3], [0, 14, 18]);
        assert_eq!(g.l_a, None);

        // bs_pointer = 1: middleBorder = L_E - 1, l_A = L_E + 1 - 1 = 3.
        f.pointer = 1;
        assert_eq!(compute_grid(&f).unwrap().l_a, Some(3));

        // bs_pointer = 3: middleBorder = 1, l_A = 1.
        f.pointer = 3;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_noise[..3], [0, 8, 18]);
        assert_eq!(g.l_a, Some(1));

        // bs_pointer > L_E points outside of the frame.
        f.pointer = 4;
        assert!(compute_grid(&f).is_err());

        // A single envelope: t_E = [0, 16 + bs_var_bord_1].
        let mut f = fields(FrameClass::FixVar, 1);
        f.var_bord_1 = 3;
        f.pointer = 1;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_env[..2], [0, 19]);
        assert_eq!((g.num_noise, &g.t_noise[..2]), (1, &[0, 19][..]));
        assert_eq!(g.l_a, Some(1));
    }

    #[test]
    fn verify_varfix() {
        // absBordLead = 1, absBordTrail = 16, n_RelLead = 2: t_E = [1, 1 + 4, 5 + 2, 16].
        let mut f = fields(FrameClass::VarFix, 3);
        f.var_bord_0 = 1;
        f.num_rel_0 = 2;
        f.rel_bord_0 = [4, 2, 0];
        f.pointer = 3;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_env[..4], [1, 5, 7, 16]);
        // middleBorder = bs_pointer - 1 = 2, l_A = bs_pointer - 1 = 2.
        assert_eq!(g.t_noise[..3], [1, 7, 16]);
        assert_eq!(g.l_a, Some(2));

        // bs_pointer = 0: middleBorder = 1, l_A = -1.
        f.pointer = 0;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_noise[..3], [1, 5, 16]);
        assert_eq!(g.l_a, None);

        // bs_pointer = 1: middleBorder = L_E - 1 = 2, l_A = -1.
        f.pointer = 1;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_noise[..3], [1, 7, 16]);
        assert_eq!(g.l_a, None);

        // bs_pointer > L_E.
        f.pointer = 4;
        assert!(compute_grid(&f).is_err());

        // Borders beyond the end of the frame: 3 + 8 + 8 > 16.
        let mut f = fields(FrameClass::VarFix, 3);
        f.var_bord_0 = 3;
        f.num_rel_0 = 2;
        f.rel_bord_0 = [8, 8, 0];
        assert!(compute_grid(&f).is_err());
    }

    #[test]
    fn verify_varvar() {
        // absBordLead = 2, absBordTrail = 17, n_RelLead = 1, n_RelTrail = 2.
        // t_E(1) = 2 + 4 = 6, t_E(2) = 17 - (2 + 6) = 9, t_E(3) = 17 - 2 = 15.
        let mut f = fields(FrameClass::VarVar, 4);
        f.var_bord_0 = 2;
        f.var_bord_1 = 1;
        f.num_rel_0 = 1;
        f.num_rel_1 = 2;
        f.rel_bord_0 = [4, 0, 0];
        f.rel_bord_1 = [2, 6, 0];
        f.pointer = 1;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_env[..5], [2, 6, 9, 15, 17]);
        // bs_pointer = 1: middleBorder = L_E - 1 = 3, l_A = L_E + 1 - 1 = 4.
        assert_eq!(g.t_noise[..3], [2, 15, 17]);
        assert_eq!(g.l_a, Some(4));

        // bs_pointer = 4: middleBorder = l_A = 1.
        f.pointer = 4;
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_noise[..3], [2, 6, 17]);
        assert_eq!(g.l_a, Some(1));

        // bs_pointer = 5 > L_E.
        f.pointer = 5;
        assert!(compute_grid(&f).is_err());

        // Overlapping borders: t_E(1) = 2 + 8 = 10 > t_E(2) = 17 - 8 = 9.
        f.pointer = 0;
        f.rel_bord_0 = [8, 0, 0];
        assert!(compute_grid(&f).is_err());

        // Five envelopes: n_RelLead = 3, n_RelTrail = 1.
        let mut f = fields(FrameClass::VarVar, 5);
        f.num_rel_0 = 3;
        f.num_rel_1 = 1;
        f.rel_bord_0 = [2, 2, 2];
        f.rel_bord_1 = [4, 0, 0];
        let g = compute_grid(&f).unwrap();
        assert_eq!(g.t_env[..6], [0, 2, 4, 6, 12, 16]);
        assert_eq!(g.t_noise[..3], [0, 12, 16]);

        // More than 5 envelopes.
        let mut f = fields(FrameClass::VarVar, 6);
        f.num_rel_0 = 3;
        f.num_rel_1 = 2;
        assert!(compute_grid(&f).is_err());
    }
}

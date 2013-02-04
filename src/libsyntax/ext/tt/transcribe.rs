// Copyright 2012 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use core::prelude::*;

use ast;
use ast::{token_tree, tt_delim, tt_tok, tt_seq, tt_nonterminal,ident};
use ast_util;
use codemap::{span, dummy_sp};
use diagnostic::span_handler;
use ext::tt::macro_parser::{named_match, matched_seq, matched_nonterminal};
use parse::token::{EOF, INTERPOLATED, IDENT, Token, nt_ident, ident_interner};
use parse::lexer::TokenAndSpan;

use core::option;
use core::vec;
use std;
use std::oldmap::HashMap;

enum tt_frame_up { /* to break a circularity */
    tt_frame_up(Option<tt_frame>)
}

/* FIXME #2811: figure out how to have a uniquely linked stack, and change to
   `~` */
///an unzipping of `token_tree`s
type tt_frame = @{
    readme: ~[ast::token_tree],
    mut idx: uint,
    dotdotdoted: bool,
    sep: Option<Token>,
    up: tt_frame_up,
};

pub type tt_reader = @{
    sp_diag: span_handler,
    interner: @ident_interner,
    mut cur: tt_frame,
    /* for MBE-style macro transcription */
    interpolations: std::oldmap::HashMap<ident, @named_match>,
    mut repeat_idx: ~[uint],
    mut repeat_len: ~[uint],
    /* cached: */
    mut cur_tok: Token,
    mut cur_span: span
};

/** This can do Macro-By-Example transcription. On the other hand, if
 *  `src` contains no `tt_seq`s and `tt_nonterminal`s, `interp` can (and
 *  should) be none. */
pub fn new_tt_reader(sp_diag: span_handler, itr: @ident_interner,
                     interp: Option<std::oldmap::HashMap<ident,@named_match>>,
                     src: ~[ast::token_tree])
                  -> tt_reader {
    let r = @{sp_diag: sp_diag, interner: itr,
              mut cur: @{readme: src, mut idx: 0u, dotdotdoted: false,
                         sep: None, up: tt_frame_up(option::None)},
              interpolations: match interp { /* just a convienience */
                None => std::oldmap::HashMap(),
                Some(x) => x
              },
              mut repeat_idx: ~[],
              mut repeat_len: ~[],
              /* dummy values, never read: */
              mut cur_tok: EOF,
              mut cur_span: dummy_sp()
             };
    tt_next_token(r); /* get cur_tok and cur_span set up */
    return r;
}

pure fn dup_tt_frame(&&f: tt_frame) -> tt_frame {
    @{readme: f.readme, mut idx: f.idx, dotdotdoted: f.dotdotdoted,
      sep: f.sep, up: match f.up {
        tt_frame_up(Some(up_frame)) => {
          tt_frame_up(Some(dup_tt_frame(up_frame)))
        }
        tt_frame_up(none) => tt_frame_up(none)
      }
     }
}

pub pure fn dup_tt_reader(&&r: tt_reader) -> tt_reader {
    @{sp_diag: r.sp_diag, interner: r.interner,
      mut cur: dup_tt_frame(r.cur),
      interpolations: r.interpolations,
      mut repeat_idx: copy r.repeat_idx, mut repeat_len: copy r.repeat_len,
      mut cur_tok: r.cur_tok, mut cur_span: r.cur_span}
}


pure fn lookup_cur_matched_by_matched(r: tt_reader,
                                      start: @named_match) -> @named_match {
    pure fn red(+ad: @named_match, idx: &uint) -> @named_match {
        match *ad {
          matched_nonterminal(_) => {
            // end of the line; duplicate henceforth
            ad
          }
          matched_seq(ads, _) => ads[*idx]
        }
    }
    vec::foldl(start, r.repeat_idx, red)
}

fn lookup_cur_matched(r: tt_reader, name: ident) -> @named_match {
    lookup_cur_matched_by_matched(r, r.interpolations.get(name))
}
enum lis {
    lis_unconstrained, lis_constraint(uint, ident), lis_contradiction(~str)
}

fn lockstep_iter_size(t: token_tree, r: tt_reader) -> lis {
    fn lis_merge(lhs: lis, rhs: lis, r: tt_reader) -> lis {
        match lhs {
          lis_unconstrained => rhs,
          lis_contradiction(_) => lhs,
          lis_constraint(l_len, l_id) => match rhs {
            lis_unconstrained => lhs,
            lis_contradiction(_) => rhs,
            lis_constraint(r_len, _) if l_len == r_len => lhs,
            lis_constraint(r_len, r_id) => {
                let l_n = *r.interner.get(l_id);
                let r_n = *r.interner.get(r_id);
                lis_contradiction(fmt!("Inconsistent lockstep iteration: \
                                       '%s' has %u items, but '%s' has %u",
                                        l_n, l_len, r_n, r_len))
            }
          }
        }
    }
    match t {
      tt_delim(ref tts) | tt_seq(_, ref tts, _, _) => {
        vec::foldl(lis_unconstrained, (*tts), |lis, tt|
            lis_merge(lis, lockstep_iter_size(*tt, r), r))
      }
      tt_tok(*) => lis_unconstrained,
      tt_nonterminal(_, name) => match *lookup_cur_matched(r, name) {
        matched_nonterminal(_) => lis_unconstrained,
        matched_seq(ads, _) => lis_constraint(ads.len(), name)
      }
    }
}


pub fn tt_next_token(&&r: tt_reader) -> TokenAndSpan {
    let ret_val = TokenAndSpan { tok: r.cur_tok, sp: r.cur_span };
    while r.cur.idx >= r.cur.readme.len() {
        /* done with this set; pop or repeat? */
        if ! r.cur.dotdotdoted
            || r.repeat_idx.last() == r.repeat_len.last() - 1 {

            match r.cur.up {
              tt_frame_up(None) => {
                r.cur_tok = EOF;
                return ret_val;
              }
              tt_frame_up(Some(tt_f)) => {
                if r.cur.dotdotdoted {
                    r.repeat_idx.pop();
                    r.repeat_len.pop();
                }

                r.cur = tt_f;
                r.cur.idx += 1u;
              }
            }

        } else { /* repeat */
            r.cur.idx = 0u;
            r.repeat_idx[r.repeat_idx.len() - 1u] += 1u;
            match r.cur.sep {
              Some(ref tk) => {
                r.cur_tok = (*tk); /* repeat same span, I guess */
                return ret_val;
              }
              None => ()
            }
        }
    }
    loop { /* because it's easiest, this handles `tt_delim` not starting
    with a `tt_tok`, even though it won't happen */
        match r.cur.readme[r.cur.idx] {
          tt_delim(ref tts) => {
            r.cur = @{readme: (*tts), mut idx: 0u, dotdotdoted: false,
                      sep: None, up: tt_frame_up(option::Some(r.cur)) };
            // if this could be 0-length, we'd need to potentially recur here
          }
          tt_tok(sp, ref tok) => {
            r.cur_span = sp; r.cur_tok = (*tok);
            r.cur.idx += 1u;
            return ret_val;
          }
          tt_seq(sp, ref tts, ref sep, zerok) => {
            match lockstep_iter_size(tt_seq(sp, (*tts), (*sep), zerok), r) {
              lis_unconstrained => {
                r.sp_diag.span_fatal(
                    sp, /* blame macro writer */
                    ~"attempted to repeat an expression containing no syntax \
                     variables matched as repeating at this depth");
              }
              lis_contradiction(ref msg) => {
                /* FIXME #2887 blame macro invoker instead*/
                r.sp_diag.span_fatal(sp, (*msg));
              }
              lis_constraint(len, _) => {
                if len == 0 {
                    if !zerok {
                        r.sp_diag.span_fatal(sp, /* FIXME #2887 blame invoker
                                                  */
                                             ~"this must repeat at least \
                                              once");
                    }

                    r.cur.idx += 1u;
                    return tt_next_token(r);
                } else {
                    r.repeat_len.push(len);
                    r.repeat_idx.push(0u);
                    r.cur = @{
                        readme: (*tts),
                        mut idx: 0u,
                        dotdotdoted: true,
                        sep: (*sep),
                        up: tt_frame_up(option::Some(r.cur))
                    };
                }
              }
            }
          }
          // FIXME #2887: think about span stuff here
          tt_nonterminal(sp, ident) => {
            match *lookup_cur_matched(r, ident) {
              /* sidestep the interpolation tricks for ident because
              (a) idents can be in lots of places, so it'd be a pain
              (b) we actually can, since it's a token. */
              matched_nonterminal(nt_ident(sn,b)) => {
                r.cur_span = sp; r.cur_tok = IDENT(sn,b);
                r.cur.idx += 1u;
                return ret_val;
              }
              matched_nonterminal(ref other_whole_nt) => {
                r.cur_span = sp; r.cur_tok = INTERPOLATED((*other_whole_nt));
                r.cur.idx += 1u;
                return ret_val;
              }
              matched_seq(*) => {
                r.sp_diag.span_fatal(
                    copy r.cur_span, /* blame the macro writer */
                    fmt!("variable '%s' is still repeating at this depth",
                         *r.interner.get(ident)));
              }
            }
          }
        }
    }

}

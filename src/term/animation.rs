use std::thread;
use std::sync::Arc;

use rand::Rng;

use super::{Term, Cell, Line, Column, Grid};

use super::super::display::Notifier;
use super::super::sync::FairMutex;
use super::super::term::cell::*;
use super::super::ansi::Color;
use super::super::Rgb;

#[derive(Clone)]
pub struct MatrixUndo {
    pub tick: u64,
    pub last_change_detected: u64,
    pub original_columns: Vec<Vec<Cell>>,
    pub columns: Vec<Vec<(Cell, bool)>>,
}

impl MatrixUndo {
    pub fn new() -> Self {
        MatrixUndo {
            tick: 0,
            last_change_detected: 0,
            original_columns: vec![],
            columns: vec![],
        }
    }
}

pub fn undo(term: &mut Term)
{
    if term.undo.columns.is_empty() {
        return;
    }
    term.undo.last_change_detected = term.undo.tick;
    let orig = &term.undo.original_columns.clone();
    let columns = &term.undo.columns.clone();
    let grid = term.grid_mut();
    let height = grid.num_lines().0;
    let width = grid.num_cols().0;
    if !orig.is_empty() {
        for col_index in 0..width {
            let col = &columns[col_index];
            for row_index in 0..height {
                let relative_index = std::cmp::max(col.len() - height, 0) + row_index;

                let (matrix_ch, _real) = columns[col_index][relative_index];
                let current_screen_buffer_ch = grid[Line(row_index)][Column(col_index)].c;
                let original_ch = orig[col_index][row_index];

                if current_screen_buffer_ch == matrix_ch.c && matrix_ch.c != original_ch.c {
                    //This char hasn't changed other than by us (probably?)
                    // - we should change it back to what it was...
                    grid[Line(row_index)][Column(col_index)] = orig[col_index][row_index];
                }
            }
        }
    }

    term.undo.columns.clear();
}


/// Trail styles that could be?:
///    * random alphanumerics (actual char at end)
///    * case switcher
///    * lazer left-right art deco criss cross????
///    * left to right refresh using underscore flag as a line that goes across....
///
fn screen_shot(grid: &Grid<Cell>) -> Vec<Vec<Cell>> {
    let mut original_columns = vec![];
    let width = grid.num_cols().0;
    let height = grid.num_lines().0;

    for col_index in 0..width {
        let mut column = Vec::new();
        for row in 0..height {
            column.push(grid[Line(row)][Column(col_index)].clone());
        }
        original_columns.push(column);
    }
    original_columns
}

/// Compare a previous snapshot to the current grid and find the lowest row for each column where
/// there is a difference.
fn calc_lowest_char_changed_per_col(grid: &Grid<Cell>, orig: &Vec<Vec<Cell>>) -> Vec<usize> {
    let height = grid.num_lines().0;
    let width = grid.num_cols().0;
    let mut lowest_char_changed_per_col = Vec::with_capacity(width);
    for col_index in 0..width {
        let mut index = height - 1;
        if !orig.is_empty() {
            let col = &orig[col_index];
            for row_index in (0..col.len()).rev() {
                if grid[Line(row_index)][Column(col_index)].c != col[row_index].c {
                    index = row_index;
                    break;//todo: functional style
                }
            }
        }
        lowest_char_changed_per_col.push(index);
    }
    lowest_char_changed_per_col
}

pub fn start_animation_thread(c_term: Arc<FairMutex<Term>>, notifier: Notifier) {
    thread::spawn(move || {
        loop {
            thread::sleep(std::time::Duration::from_millis(40));//lower this as height increases...
            // Process input and window events
            {
                let mut term = (*c_term).lock();
                term.undo.tick += 1;
                let width = term.grid().num_cols().0;
                let height = term.grid().num_lines().0;

                if !term.undo.columns.is_empty() {
                    let has_been_resized = term.undo.columns.len() != width ||
                        term.undo.columns[0].iter().filter(|(_ch, real)| *real).count() != height;

                    if has_been_resized {
                        //RESET
                        //term_lock.undo(); - would be nice but undo would need to deal with resize.
                        term.undo.columns.clear();
                        term.undo.original_columns = screen_shot(term.grid());
                    }
                }

                if term.undo.columns.is_empty() && term.undo.last_change_detected + 4 <= term.undo.tick {
                    let lowest_char_changed_per_col = calc_lowest_char_changed_per_col(
                        term.grid(), &term.undo.original_columns);

                    //Must be set after calc lowest char......
                    term.undo.original_columns = screen_shot(term.grid());

                    term.undo.columns = generate_animation_script(&mut term, &lowest_char_changed_per_col)
                }

                step(&mut term);

                notifier.notify();
                term.dirty = true;
            }
        }
    });
}

//
// Below are functions specific to the matrix effect.
//

fn generate_animation_script(term: &Term, lowest_char_changed_per_col: &Vec<usize>)
                             -> Vec<Vec<(Cell, bool)>>
{
    let width = term.grid().num_cols().0;
    let height = term.grid().num_lines().0;
    let mut results = vec![];
    for col_index in 0..width {
        let mut column = Vec::new();

        for row_index in 0..height {
            let cell = term.grid()[Line(row_index)][Column(col_index)];
            column.push((cell.clone(), true));

            //Add random chars...
            if cell.c != ' ' && row_index < lowest_char_changed_per_col[col_index]
            {
                //TODO less random chars if many chars on that column relative to spaces....
                let ran_char_count = rand::thread_rng().gen_range(2, 10);
                for i in 0..ran_char_count
                    {
                        let ch_int: u8 = rand::thread_rng()
                            .gen_range(31, 126);

                        let mut rnd_char = Cell::new(ch_int as char,
                                                     Color::Spec(Rgb {
                                                         r: 0,
                                                         g: (150 + (ran_char_count - i) * 10),
                                                         b: 0,
                                                     }),
                                                     cell.bg);

                        if rand::thread_rng().gen_bool(0.2) {
                            rnd_char.flags = rnd_char.flags | Flags::BOLD;
                        }

                        column.push((rnd_char, false));
                    }

                //Char Gap:
                for _ in 0..rand::thread_rng().gen_range(2, 8) {
                    let space = Cell::new(' ', cell.fg, cell.bg);
                    column.push((space, false));
                }
            }
        }
        results.push(column);
    }
    results
}

fn step(term: &mut Term) -> () {
    let width = term.grid().num_cols().0;
    let height = term.grid().num_lines().0;
    let mut unreal_char_found = false;
    for col in &mut *term.undo.columns {
        let mut index: usize = col.len() - 1;
        for (_ch, real) in col.iter().rev() {
            if !real || index == 0 {
                if !real {
                    unreal_char_found = true;
                }
                break;
            }
            index -= 1;
        }

        if index > 0 {
            col.remove(index);
            //All the column above this char should be shifted down one...
        }
    }
    if unreal_char_found {
        //Update grid to be the chars found at the bottom of term.undo.columns.
        for col_index in 0..width {
            let col_len = &term.undo.columns[col_index].len();
            for row in 0..height {
                let relative_index = (col_len - height) + row;
                let (ch, _real) = term.undo.columns[col_index][relative_index];
                let cell = &term.grid()[Line(row)][Column(col_index)];
                if cell.c != ch.c {
                    term.grid_mut()[Line(row)][Column(col_index)] = ch;
                }
            }
        }
    }
}
// Copyright 2016 Joe Wilm, The Alacritty Project Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
//! Alacritty - The GPU Enhanced Terminal
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use, clippy::wrong_pub_self_convention)]
#![cfg_attr(feature = "nightly", feature(core_intrinsics))]
#![cfg_attr(all(test, feature = "bench"), feature(test))]

// With the default subsystem, 'console', windows creates an additional console
// window for the program.
// This is silently ignored on non-windows systems.
// See https://msdn.microsoft.com/en-us/library/4cc7ya5b.aspx for more details.
#![windows_subsystem = "windows"]

#[cfg(target_os = "macos")]
use dirs;

#[cfg(windows)]
use winapi::um::wincon::{AttachConsole, FreeConsole, ATTACH_PARENT_PROCESS};

use log::{info, error};

use rand::Rng;

use std::error::Error;
use std::sync::Arc;
use std::thread;

#[cfg(target_os = "macos")]
use std::env;

#[cfg(not(windows))]
use std::os::unix::io::AsRawFd;

#[cfg(target_os = "macos")]
use alacritty::locale;
use alacritty::{cli, event, die};
use alacritty::config::{self, Config, Error as ConfigError};
use alacritty::display::Display;
use alacritty::event_loop::{self, EventLoop, Msg};
use alacritty::logging::{self, LoggerProxy};
use alacritty::panic;
use alacritty::sync::FairMutex;
use alacritty::term::Term;
use alacritty::tty::{self, process_should_exit};
use alacritty::util::fmt::Red;
use alacritty::index::{Line, Column};
use alacritty::Grid;
use alacritty::term::Cell;

fn main() {
    panic::attach_handler();

    // When linked with the windows subsystem windows won't automatically attach
    // to the console of the parent process, so we do it explicitly. This fails
    // silently if the parent has no console.
    #[cfg(windows)]
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS); }

    // Load command line options
    let options = cli::Options::load();

    // Initialize the logger as soon as possible as to capture output from other subsystems
    let logger_proxy = logging::initialize(&options).expect("Unable to initialize logger");

    // Load configuration file
    let config = load_config(&options).update_dynamic_title(&options);

    // Switch to home directory
    #[cfg(target_os = "macos")]
    env::set_current_dir(dirs::home_dir().unwrap()).unwrap();
    // Set locale
    #[cfg(target_os = "macos")]
    locale::set_locale_environment();

    // Run alacritty
    if let Err(err) = run(config, &options, logger_proxy) {
        die!("Alacritty encountered an unrecoverable error:\n\n\t{}\n", Red(err));
    }
}

/// Load configuration
///
/// If a configuration file is given as a command line argument we don't
/// generate a default file. If an empty configuration file is given, i.e.
/// /dev/null, we load the compiled-in defaults.)
fn load_config(options: &cli::Options) -> Config {
    let config_path = options.config_path()
        .or_else(Config::installed_config)
        .or_else(|| Config::write_defaults().ok());

    if let Some(config_path) = config_path {
        Config::load_from(&*config_path).unwrap_or_else(|err| {
            match err {
                ConfigError::Empty => info!("Config file {:?} is empty; loading default", config_path),
                _ => error!("Unable to load default config: {}", err),
            }

            Config::default()
        })
    } else {
        error!("Unable to write the default config");
        Config::default()
    }
}

fn first_col(grid: &Grid<Cell>, debug_col: usize) -> Vec<char> {
    let mut vec = Vec::new();
    let height = grid.num_lines().0;
    for row_index in 0..height {
        vec.push(grid[Line(row_index)][Column(debug_col)].c);
    }
    vec
}


/// Plan
///
///
/// landed = original_snapshot []
/// ^
/// |
/// | Diff: chars to update...
/// |
/// V
/// updated_snapshot []
/// (gradually landed => updated_snapshot row by row from bottom upwards...)
///
///
///
/// overlay vector per column... 0=alpha channel, use progress where alpha
///
///
///
///
/// If change detected, wait x ticks to ensure no more changes coming through....
///
/// Trail styles:
///    * random alphanumerics (actual char at end)
///    * case switcher
///    * lazer left-rigth art deco criss cross????
///    * left to right refresh using underscore as a line that goes across....
///
fn screen_shot(grid: &Grid<Cell>) -> Vec<Vec<Cell>> {
    let mut original_columns = vec![];
    println!("initialising");
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
    let mut lowest_char_changed_per_col = Vec::with_capacity(orig.len());
    for col_index in 0..orig.len() {
        let col = &orig[col_index];
        let mut index = 0;//col.len();
        for row_index in (0..col.len()).rev() {
            if grid[Line(row_index)][Column(col_index)].c != col[row_index].c {
                index = row_index;
                break;//todo: functional style
            }
        }
        lowest_char_changed_per_col.push(index);
    }
    lowest_char_changed_per_col
}

/// Run Alacritty
///
/// Creates a window, the terminal state, pty, I/O event loop, input processor,
/// config change monitor, and runs the main display loop.
fn run(
    mut config: Config,
    options: &cli::Options,
    mut logger_proxy: LoggerProxy,
) -> Result<(), Box<dyn Error>> {
    info!("Welcome to Alacritty");
    if let Some(config_path) = config.path() {
        info!("Configuration loaded from {:?}", config_path.display());
    };

    // Set environment variables
    tty::setup_env(&config);

    // Create a display.
    //
    // The display manages a window and can draw the terminal
    let mut display = Display::new(&config, options, logger_proxy.clone())?;

    info!(
        "PTY Dimensions: {:?} x {:?}",
        display.size().lines(),
        display.size().cols()
    );

    // Create the terminal
    //
    // This object contains all of the state about what's being displayed. It's
    // wrapped in a clonable mutex since both the I/O loop and display need to
    // access it.
    let mut terminal = Term::new(&config, display.size().to_owned());
    terminal.set_logger_proxy(logger_proxy.clone());
    let terminal = Arc::new(FairMutex::new(terminal));


    // Find the window ID for setting $WINDOWID
    let window_id = display.get_window_id();

    // Create the pty
    //
    // The pty forks a process to run the shell on the slave side of the
    // pseudoterminal. A file descriptor for the master side is retained for
    // reading/writing to the shell.
    let pty = tty::new(&config, options, &display.size(), window_id);

    // Get a reference to something that we can resize
    //
    // This exists because rust doesn't know the interface is thread-safe
    // and we need to be able to resize the PTY from the main thread while the IO
    // thread owns the EventedRW object.
    #[cfg(windows)]
    let mut resize_handle = pty.resize_handle();
    #[cfg(not(windows))]
    let mut resize_handle = pty.fd.as_raw_fd();

    // Create the pseudoterminal I/O loop
    //
    // pty I/O is ran on another thread as to not occupy cycles used by the
    // renderer and input processing. Note that access to the terminal state is
    // synchronized since the I/O loop updates the state, and the display
    // consumes it periodically.
    let event_loop = EventLoop::new(
        Arc::clone(&terminal),
        display.notifier(),
        pty,
        options.ref_test,
    );

    // The event loop channel allows write requests from the event processor
    // to be sent to the loop and ultimately written to the pty.
    let loop_tx = event_loop.channel();

    // Event processor
    //
    // Need the Rc<RefCell<_>> here since a ref is shared in the resize callback
    let mut processor = event::Processor::new(
        event_loop::Notifier(event_loop.channel()),
        display.resize_channel(),
        options,
        &config,
        options.ref_test,
        display.size().to_owned(),
    );

    // Create a config monitor when config was loaded from path
    //
    // The monitor watches the config file for changes and reloads it. Pending
    // config changes are processed in the main loop.
    let config_monitor = match (options.live_config_reload, config.live_config_reload()) {
        // Start monitor if CLI flag says yes
        (Some(true), _) |
        // Or if no CLI flag was passed and the config says yes
        (None, true) => config.path()
                .map(|path| config::Monitor::new(path, display.notifier())),
        // Otherwise, don't start the monitor
        _ => None,
    };

    // Kick off the I/O thread
    let _io_thread = event_loop.spawn(None);

    info!("Initialisation complete");

    let c_term = terminal.clone();
    let notifier = display.notifier();
    thread::spawn(move || {
        let mut columns : Vec<Vec<(Cell, bool)>> = vec![];
        let mut snapshots : Vec<Vec<Vec<Cell>>>= vec![];
        let mut original_columns = None;
        let mut tick : u64 = 0;
        let mut last_change_detected : u64 = 0;

        let debug_col = 3;
        loop {
            tick += 1;//TODO tick overflow
            thread::sleep(std::time::Duration::from_millis(30));//lower this as height increases...
            // Process input and window events
            {
                let mut term_lock = (*c_term).lock();
                {
                    if columns.is_empty() {
                        let grid: &mut Grid<Cell> = term_lock.grid_mut();//TODO: use   self.grid.region_mut(..).each(|c| c...);
                        original_columns = Some(screen_shot(grid));
                        println!("initi {:?}", original_columns.clone().unwrap()[debug_col]);
                    }

//                    if let Some(original_columns2) = original_columns {
//                        println!("initialising-undo");
//                        term_lock.undo = Some(alacritty::term::MatrixUndo{ original_columns:original_columns2});
////                        //RESET
////                        columns.clear();
////                        original_columns = None;
////                        snapshots.clear();
//                    }

                    let grid = term_lock.grid_mut();//TODO: use   self.grid.region_mut(..).each(|c| c...);
                    let width = grid.num_cols().0;
                    let height = grid.num_lines().0;
                    let mut lowest_char_changed_per_col = vec![];
                    for _ in 0..width {
                        lowest_char_changed_per_col.push(0);
                    }

                    if !columns.is_empty() {
                        //is same size?
                        let has_been_resized = columns.len() != width ||
                            columns[0].iter().filter(|(_ch, real)| *real).count() != height;

                        if has_been_resized {
                            //RESET
                            columns.clear();
                            original_columns = None;
                            snapshots.clear();
                        }
                    }

                    if !columns.is_empty() {
                        let mut dirty = false;
                        //Are the expected values still there? or is there new data...
                        for col_index in 0..width {
                            let col = &columns[col_index];
                            for row in 0..height {
                                let relative_index = (col.len() - height) + row;
                                //    println!("r{},c{}", relative_index, col_index);
                                let (ch, _real) = columns[col_index][relative_index];
                                if grid[Line(row)][Column(col_index)].c != ch.c {
                                    dirty = true;
                                    break;//could break out of outer loop also
                                }
                            }
                        }
                        if dirty {
                            //Using UNDO rather than this..
                            //Undo our changes!
                            println!("change detected!");
                            last_change_detected = tick;
                            if let Some(orig) = &original_columns {
                                println!("origi: {:?}", &orig[debug_col]);
                                println!("scren: {:?}", first_col(grid, debug_col));
                                for col_index in 0..width {
                                    let col = &columns[col_index];
                                    for row_index in 0..height {
                                        let relative_index = (col.len() - height) + row_index;
                                        //    println!("r{},c{}", relative_index, col_index);

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

                            //Any changes left should be changes that we want to represent... between grid and orig.
                         //   println!("scre2: {:?}", first_col(grid,debug_col));
                            let screen = screen_shot(grid);
                            original_columns = Some(screen);
                            //when multiple changes come in rapid procession....
                           // println!("origi: {:?}", original_columns.unwrap()[debug_col]);
                            columns.clear()
                        }
                    }

                    if columns.is_empty() && last_change_detected + 2 <= tick {
                        println!("setup random chars...");
                        lowest_char_changed_per_col = if snapshots.is_empty() {
                            let mut lowest_char_changed_per_col = vec![];
                            for _ in 0..width {
                                lowest_char_changed_per_col.push(0);
                            }
                            lowest_char_changed_per_col
                        }
                        else {
                            calc_lowest_char_changed_per_col(&grid, &snapshots[0])
                        };
                        snapshots.clear();

                        for col_index in 0..width {
                            let mut column = Vec::new();

                            let mut interesting_chars = 0;
                            for row_index in 0..lowest_char_changed_per_col[col_index] {
                                let ch = grid[Line(row_index)][Column(col_index)].c;
                                if ch != ' ' { interesting_chars += 1 }
                            }
                            let work_ratio =  height / (std::cmp::max(interesting_chars, 1) * 2);

                            for row_index in 0..height {
                                let cell = grid[Line(row_index)][Column(col_index)];
                                column.push((cell.clone(), true));

                                //Add random chars...
                                if cell.c != ' ' && row_index < lowest_char_changed_per_col[col_index] {
                                    //TODO less random chars if many chars on that column relative to spaces....
                                    let ran_char_count = rand::thread_rng().gen_range(2, std::cmp::max(10, 3));
                                    for i in 0..ran_char_count
                                    {
                                        let ch_int: u8 = rand::thread_rng()
                                            .gen_range(31, 126);
                                        let mut rnd_char = Cell::new(ch_int as char,
                                                                     alacritty::ansi::Color::Spec(alacritty::Rgb{r:0, g:(150 + (ran_char_count-i) * 10),b:0}),
                                                                     cell.bg);

                                        if rand::thread_rng().gen_bool(0.2) {
                                            use alacritty::term::cell::*;
                                            rnd_char.flags = rnd_char.flags | Flags::BOLD; //todo this is bold...
                                        }

                                        column.push((rnd_char, false));
                                    }

                                    //Char Gap:
                                    for _ in 0..rand::thread_rng().gen_range(2, std::cmp::max(8,3)) {
                                        let space = Cell::new(' ', cell.fg, cell.bg);
                                        column.push((space, false));
                                    }
                                }
                            }
                            columns.push(column);
                        }
                        println!("prep done");
                    }

                    //Step
                    let mut found = false;
                    for col in &mut columns {
                        let mut index : usize = col.len() - 1;
                        for (_ch, real) in col.iter().rev() {
                            if !real || index == 0 {
                                if !real {
                                    found = true;
                                }
                                break;
                            }
                            index -= 1;
                        }

                        if index > 0 {
                            //rather than remove index, we reduce screen churn if we remove the first random one....
                            let idx = index;

                            //Didn's seem to help much...
//                            for i in (0..idx).rev() {
//                                let (_ch, real) = col[i];
//                                if real {
//                                    idx = i + 1;
//                                    break;
//                                }
//                            }

                            col.remove(idx);
                        }
                    }

                    if found {
                        for col_index in 0..width {
                            let col = &columns[col_index];
                            for row in 0..height {
                                let relative_index = (col.len() - height) + row;
                                let (ch, _real) = columns[col_index][relative_index];
                                grid[Line(row)][Column(col_index)] = ch;
                            }
                        }
                    } else {
                        if snapshots.is_empty() {
                            //record the resting state, that we can calc diffs from it.
                            snapshots.push(screen_shot(grid));
                        }
                    }
                }

                notifier.notify();
                term_lock.dirty = true;
            }

        }
    });

    // Main display loop
    loop {
        // Process input and window events
        let mut terminal_lock = processor.process_events(&terminal, display.window());
        // Handle config reloads
        if let Some(new_config) = config_monitor
            .as_ref()
            .and_then(|monitor| monitor.pending_config())
        {
            config = new_config.update_dynamic_title(options);
            display.update_config(&config);
            processor.update_config(&config);
            terminal_lock.update_config(&config);
            terminal_lock.dirty = true;
        }


        // Maybe draw the terminal
        if terminal_lock.needs_draw() {
            // Try to update the position of the input method editor
            #[cfg(not(windows))]
            display.update_ime_position(&terminal_lock);

            // Handle pending resize events
            //
            // The second argument is a list of types that want to be notified
            // of display size changes.
            display.handle_resize(&mut terminal_lock, &config, &mut [&mut resize_handle, &mut processor]);

            drop(terminal_lock);

            // Draw the current state of the terminal
            display.draw(&terminal, &config);
        }

        // Begin shutdown if the flag was raised.
        if process_should_exit() {
            break;
        }
    }

    loop_tx
        .send(Msg::Shutdown)
        .expect("Error sending shutdown to event loop");

    // FIXME patch notify library to have a shutdown method
    // config_reloader.join().ok();

    // Without explicitly detaching the console cmd won't redraw it's prompt
    #[cfg(windows)]
    unsafe { FreeConsole(); }

    info!("Goodbye");

    if !options.persistent_logging && !config.persistent_logging() {
        logger_proxy.delete_log();
    }

    Ok(())
}

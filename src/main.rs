#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{delay::Delay, gpio::{Io, Input, Level, Output, OutputOpenDrain, Pull}, prelude::*};
use esp_hal::i2c::I2c;
use sh1106::{prelude::*, Builder};
use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::*,
};
use u8g2_fonts::{fonts, FontRenderer};
use dht_sensor::{dht11, DhtReading};
use core::fmt::Write;
use heapless::String;

// aktywny widok
enum Screen {
    Weather,
    Sort,
}

// ikonka termometru 12x12
const THERMOMETER: [u16; 12] = [
    0b0001100000000000,
    0b0010010000000000,
    0b0010010000000000,
    0b0010110000000000,
    0b0010110000000000,
    0b0010110000000000,
    0b0010110000000000,
    0b0100111000000000,
    0b0101111000000000,
    0b0100111000000000,
    0b0011110000000000,
    0b0000000000000000,
];

// ikonka kropli wody 12x12
const DROP: [u16; 12] = [
    0b0001000000000000,
    0b0001000000000000,
    0b0011100000000000,
    0b0011100000000000,
    0b0111110000000000,
    0b0111110000000000,
    0b1111111000000000,
    0b1111111000000000,
    0b1111111000000000,
    0b0111110000000000,
    0b0011100000000000,
    0b0000000000000000,
];

// ikonka zegara 12x12
const CLOCK: [u16; 12] = [
    0b0001110000000000,
    0b0110001100000000,
    0b0100000100000000,
    0b1000100010000000,
    0b1000100010000000,
    0b1000111010000000,
    0b1000000010000000,
    0b1000000010000000,
    0b0100000100000000,
    0b0110001100000000,
    0b0001110000000000,
    0b0000000000000000,
];

// rysuje ikonke 12x12 na podanej pozycji
fn draw_icon<DI>(display: &mut GraphicsMode<DI>, icon: &[u16; 12], x_off: u32, y_off: u32)
where
    DI: sh1106::interface::DisplayInterface,
{
    for (y, row) in icon.iter().enumerate() {
        for x in 0..12 {
            if row & (1 << (15 - x)) != 0 {
                display.set_pixel(x_off + x as u32, y_off + y as u32, 1);
            }
        }
    }
}

// prosty generator pseudolosowy LCG (nie potrzeba zewnetrznego cratea)
fn lcg_next(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed
}

// generuje tablice 64 pseudolosowych wartosci od 1 do 51
fn gen_array(seed: &mut u32) -> [u8; 64] {
    let mut arr = [0u8; 64];
    for i in 0..64 {
        arr[i] = (lcg_next(seed) % 51 + 1) as u8;
    }
    arr
}

// rysuje slupki — active_a i active_b to indeksy aktualnie porownywanych elementow (rysowane inaczej)
fn draw_bars<DI>(display: &mut GraphicsMode<DI>, arr: &[u8; 64], active_a: usize, active_b: usize)
where
    DI: sh1106::interface::DisplayInterface,
{
    for i in 0..64 {
        let x = (i * 2) as u32;
        let h = arr[i] as u32;
        let is_active = i == active_a || i == active_b;

        for y in 0..h {
            let py = 63 - y;
            if is_active {
                // aktywne slupki rysujemy tylko jako krawedzie (pusty srodek)
                if y == 0 || y == h - 1 || x == 0 || x == 1 {
                    display.set_pixel(x, py, 1);
                    display.set_pixel(x + 1, py, 1);
                }
            } else {
                display.set_pixel(x, py, 1);
                display.set_pixel(x + 1, py, 1);
            }
        }
    }
}

#[entry]
fn main() -> ! {
    #[allow(unused)]
    let peripherals = esp_hal::init(esp_hal::Config::default());

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    let i2c = I2c::new(peripherals.I2C0, io.pins.gpio4, io.pins.gpio5, 400.kHz());

    // wyswietlacz SH1106 po I2C
    let mut display: GraphicsMode<_> = Builder::new().connect_i2c(i2c).into();
    display.init().unwrap();
    display.clear();

    // LED na GPIO0
    let mut led = Output::new(io.pins.gpio0, Level::Low);

    // przycisk na GPIO2, pull-up (wcisniety = Low)
    let btn = Input::new(io.pins.gpio2, Pull::Up);

    // pin DHT11 jako open-drain na GPIO6
    let mut dht_pin = OutputOpenDrain::new(io.pins.gpio6, Level::High, Pull::Up);
    let mut delay = Delay::new();

    // font z polskimi znakami
    let font = FontRenderer::new::<fonts::u8g2_font_unifont_t_polish>();

    // zegar startowy - ustaw swoja godzine
    let mut hours: u8 = 14;
    let mut minutes: u8 = 30;
    let mut seconds: u8 = 0;

    // seed do generatora pseudolosowego
    let mut rng_seed: u32 = 12345;

    // aktywny widok
    let mut screen = Screen::Weather;

    // poprzedni stan przycisku — do detekcji zbocza (unikamy wielokrotnego przelaczenia)
    let mut btn_prev_low = false;

    esp_println::logger::init_logger_from_env();

    loop {
        // sprawdzenie przycisku — przelaczamy widok przy wcisnięciu (zbocze opadajace)
        let btn_now_low = btn.is_low();
        log::info!("przycisk: {}", btn_now_low);
        if btn_now_low && !btn_prev_low {
            log::info!("PRZELACZAM WIDOK");
            screen = match screen {
                Screen::Weather => Screen::Sort,
                Screen::Sort => Screen::Weather,
            };
        }
        btn_prev_low = btn_now_low;

        match screen {
            Screen::Weather => {
                // odczyt temperatury i wilgotnosci z DHT11
                let (temp, hum) = match dht11::Reading::read(&mut delay, &mut dht_pin) {
                    Ok(reading) => {
                        led.set_high();
                        (reading.temperature, reading.relative_humidity)
                    }
                    Err(_) => {
                        led.set_low();
                        (0, 0)
                    }
                };

                // formatowanie temperatury
                let mut temp_str: String<16> = String::new();
                let _ = write!(temp_str, "{}C", temp);

                // formatowanie wilgotnosci
                let mut hum_str: String<16> = String::new();
                let _ = write!(hum_str, "{}%", hum);

                // formatowanie zegara
                let mut time_str: String<16> = String::new();
                let _ = write!(time_str, "{:02}:{:02}", hours, minutes);

                display.clear();

                draw_icon(&mut display, &THERMOMETER, 2, 2);
                font.render_aligned(
                    temp_str.as_str(),
                    Point::new(16, 2),
                    u8g2_fonts::types::VerticalPosition::Top,
                    u8g2_fonts::types::HorizontalAlignment::Left,
                    u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
                    &mut display,
                ).ok();

                draw_icon(&mut display, &DROP, 66, 2);
                font.render_aligned(
                    hum_str.as_str(),
                    Point::new(80, 2),
                    u8g2_fonts::types::VerticalPosition::Top,
                    u8g2_fonts::types::HorizontalAlignment::Left,
                    u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
                    &mut display,
                ).ok();

                draw_icon(&mut display, &CLOCK, 2, 28);
                font.render_aligned(
                    time_str.as_str(),
                    Point::new(16, 28),
                    u8g2_fonts::types::VerticalPosition::Top,
                    u8g2_fonts::types::HorizontalAlignment::Left,
                    u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
                    &mut display,
                ).ok();

                display.flush().unwrap();

                delay.delay_millis(2000);

                // aktualizacja zegara
                seconds += 2;
                if seconds >= 60 {
                    seconds -= 60;
                    minutes += 1;
                    if minutes >= 60 {
                        minutes = 0;
                        hours += 1;
                        if hours >= 24 {
                            hours = 0;
                        }
                    }
                }
            }

            Screen::Sort => {
                // czekamy az przycisk zostanie zwolniony zanim zaczniemy sortowanie
                while btn.is_low() {
                    delay.delay_millis(10);
                }

                // generujemy nowa tablice i sortujemy w kółko
                let mut arr = gen_array(&mut rng_seed);
                let mut interrupted = false;

                // quicksort iteracyjny ze stosem o stalym rozmiarze
                let mut stack = [(0usize, 0usize); 64];
                let mut stack_top: usize = 0;
                stack[stack_top] = (0, 63);
                stack_top += 1;

                'sort: while stack_top > 0 {
                    stack_top -= 1;
                    let (lo, hi) = stack[stack_top];

                    if lo >= hi {
                        continue;
                    }

                    // partycjonowanie — pivot to ostatni element
                    let pivot = arr[hi] as usize;
                    let mut i = lo;

                    for j in lo..hi {
                        // sprawdz przycisk — jesli wcisniety, przerwij sortowanie
                        if btn.is_low() {
                            screen = Screen::Weather;
                            interrupted = true;
                            break 'sort;
                        }

                        if (arr[j] as usize) <= pivot {
                            // zamiana arr[i] i arr[j]
                            arr.swap(i, j);

                            // przerysuj ekran po zamianie
                            display.clear();
                            font.render_aligned(
                                "Quick Sort",
                                Point::new(0, 0),
                                u8g2_fonts::types::VerticalPosition::Top,
                                u8g2_fonts::types::HorizontalAlignment::Left,
                                u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
                                &mut display,
                            ).ok();
                            draw_bars(&mut display, &arr, i, j);
                            display.flush().unwrap();

                            delay.delay_millis(30);
                            i += 1;
                        }
                    }

                    // ostatnia zamiana — pivot na swoje miejsce
                    arr.swap(i, hi);

                    display.clear();
                    font.render_aligned(
                        "Quick Sort",
                        Point::new(0, 0),
                        u8g2_fonts::types::VerticalPosition::Top,
                        u8g2_fonts::types::HorizontalAlignment::Left,
                        u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
                        &mut display,
                    ).ok();
                    draw_bars(&mut display, &arr, i, i);
                    display.flush().unwrap();

                    delay.delay_millis(30);

                    // dodaj podtablice na stos
                    if i > lo && stack_top < 63 {
                        stack[stack_top] = (lo, i - 1);
                        stack_top += 1;
                    }
                    if i < hi && stack_top < 63 {
                        stack[stack_top] = (i + 1, hi);
                        stack_top += 1;
                    }
                }

                if !interrupted {
                    // sortowanie skonczone — pokaz posortowana tablice chwile, potem losuj od nowa
                    display.clear();
                    font.render_aligned(
                        "Quick Sort",
                        Point::new(0, 0),
                        u8g2_fonts::types::VerticalPosition::Top,
                        u8g2_fonts::types::HorizontalAlignment::Left,
                        u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
                        &mut display,
                    ).ok();
                    draw_bars(&mut display, &arr, 64, 64); // 64 = brak aktywnych
                    display.flush().unwrap();
                    delay.delay_millis(1000);
                }
            }
        }
    }
}

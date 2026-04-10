#![no_std]
#![no_main]

use core::fmt::Write;
use dht_sensor::{dht11, DhtReading};
use display_interface_spi::SPIInterface;
use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::spi::master::Spi;
use esp_hal::spi::SpiMode;
use esp_hal::{
    delay::Delay,
    gpio::{Input, Io, Level, Output, OutputOpenDrain, Pull},
    prelude::*,
};
use heapless::String;
use mipidsi::models::ST7789;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use mipidsi::Builder as DisplayBuilder;
use u8g2_fonts::{fonts, FontRenderer};

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

// rysuje ikonke 12x12 na podanej pozycji (biala na czarnym tle)
fn draw_icon<D>(display: &mut D, icon: &[u16; 12], x_off: i32, y_off: i32)
where
    D: DrawTarget<Color = Rgb565>,
{
    for (y, row) in icon.iter().enumerate() {
        for x in 0..12 {
            if row & (1 << (15 - x)) != 0 {
                let _ = Pixel(
                    Point::new(x_off + x as i32, y_off + y as i32),
                    Rgb565::WHITE,
                )
                .draw(display);
            }
        }
    }
}

// prosty generator pseudolosowy LCG (nie potrzeba zewnetrznego cratea)
fn lcg_next(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    *seed
}

// generuje tablice pseudolosowych wartosci od 1 do 120
fn gen_array(seed: &mut u32) -> [u8; NUM_BARS] {
    let mut arr = [0u8; NUM_BARS];
    for i in 0..NUM_BARS {
        arr[i] = (lcg_next(seed) % BAR_MAX_H as u32 + 1) as u8;
    }
    arr
}

const NUM_BARS: usize = 20;
const BAR_WIDTH: usize = 10;
const BAR_GAP: usize = 2;
const BAR_BOTTOM: i32 = 134;
const BAR_MAX_H: i32 = 95;
const BAR_TOP: i32 = BAR_BOTTOM - BAR_MAX_H; // 14

fn value_color(val: u8) -> Rgb565 {
    let v = val.saturating_sub(1);
    match v {
        0..=23 => Rgb565::new(31, (v as u16 * 63 / 23) as u8, 0),
        24..=47 => Rgb565::new((31 - (v - 24) as u16 * 31 / 23) as u8, 63, 0),
        48..=71 => Rgb565::new(0, 63, ((v - 48) as u16 * 31 / 23) as u8),
        72..=95 => Rgb565::new(0, (63 - (v - 72) as u16 * 63 / 23) as u8, 31),
        _ => Rgb565::new(((v - 96) as u16 * 31 / 23) as u8, 0, 31),
    }
}

// rysuje JEDEN slupek — czysci slot + rysuje kolorowy prostokat
fn draw_bar<D>(display: &mut D, arr: &[u8; NUM_BARS], index: usize, active: bool)
where
    D: DrawTarget<Color = Rgb565>,
{
    let x = (index * (BAR_WIDTH + BAR_GAP)) as i32;
    let h = arr[index] as i32;

    // czyscimy slot (czarny)
    let _ = Rectangle::new(
        Point::new(x, BAR_TOP),
        Size::new(BAR_WIDTH as u32, BAR_MAX_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
    .draw(display);

    // rysujemy slupek
    let color = if active {
        Rgb565::WHITE
    } else {
        value_color(arr[index])
    };

    let _ = Rectangle::new(
        Point::new(x, BAR_BOTTOM - h),
        Size::new(BAR_WIDTH as u32, h as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(color))
    .draw(display);
}

// rysuje wszystkie slupki (uzyj na poczatku i na koncu sortowania)
fn draw_all_bars<D>(display: &mut D, arr: &[u8; NUM_BARS])
where
    D: DrawTarget<Color = Rgb565>,
{
    for i in 0..NUM_BARS {
        draw_bar(display, arr, i, false);
    }
}

#[entry]
fn main() -> ! {
    #[allow(unused)]
    let peripherals = esp_hal::init(esp_hal::Config::default());

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

    // SPI do wyswietlacza LCD
    let sclk = io.pins.gpio4;
    let mosi = io.pins.gpio5;
    let spi = Spi::new(peripherals.SPI2, 40.MHz(), SpiMode::Mode0)
        .with_sck(sclk)
        .with_mosi(mosi);

    let cs = Output::new(io.pins.gpio7, Level::High);
    let dc = Output::new(io.pins.gpio3, Level::Low);
    let mut rst = Output::new(io.pins.gpio8, Level::High);

    // podswietlenie wlaczone
    let _bl = Output::new(io.pins.gpio10, Level::High);

    // SPI -> SpiDevice (ExclusiveDevice dodaje obsluge CS)
    let spi_dev = ExclusiveDevice::new_no_delay(spi, cs).unwrap();
    let spi_iface = SPIInterface::new(spi_dev, dc);

    let mut delay = Delay::new();

    // inicjalizacja wyswietlacza ST7789 240x135
    let mut display = DisplayBuilder::new(ST7789, spi_iface)
        .display_size(135, 240)
        .display_offset(52, 40)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .invert_colors(ColorInversion::Inverted)
        .reset_pin(rst)
        .init(&mut delay)
        .unwrap();

    // LED na GPIO0
    let mut led = Output::new(io.pins.gpio0, Level::Low);

    // przycisk na GPIO2, pull-up (wcisniety = Low)
    let btn = Input::new(io.pins.gpio2, Pull::Up);

    // pin DHT11 jako open-drain na GPIO6
    let mut dht_pin = OutputOpenDrain::new(io.pins.gpio6, Level::High, Pull::Up);

    // font maly (tytuly, etykiety)
    let font = FontRenderer::new::<fonts::u8g2_font_unifont_t_polish>();
    // font duzy (wartosci pogodowe)
    let font_big = FontRenderer::new::<fonts::u8g2_font_logisoso28_tr>();

    // zegar startowy - ustaw swoja godzine
    let mut hours: u8 = 14;
    let mut minutes: u8 = 30;
    let mut seconds: u8 = 0;

    // seed do generatora pseudolosowego
    let mut rng_seed: u32 = 12345;

    // aktywny widok
    let mut screen = Screen::Weather;

    esp_println::logger::init_logger_from_env();

    loop {
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

                display.clear(Rgb565::BLACK).ok();

                // temperatura — ikonka + duzy tekst
                draw_icon(&mut display, &THERMOMETER, 6, 10);
                font_big
                    .render_aligned(
                        temp_str.as_str(),
                        Point::new(24, 4),
                        u8g2_fonts::types::VerticalPosition::Top,
                        u8g2_fonts::types::HorizontalAlignment::Left,
                        u8g2_fonts::types::FontColor::Transparent(Rgb565::new(31, 0, 0)),
                        &mut display,
                    )
                    .ok();

                // wilgotnosc — ikonka + duzy tekst
                draw_icon(&mut display, &DROP, 130, 10);
                font_big
                    .render_aligned(
                        hum_str.as_str(),
                        Point::new(148, 4),
                        u8g2_fonts::types::VerticalPosition::Top,
                        u8g2_fonts::types::HorizontalAlignment::Left,
                        u8g2_fonts::types::FontColor::Transparent(Rgb565::new(0, 32, 31)),
                        &mut display,
                    )
                    .ok();

                // zegar — ikonka + duzy tekst, wycentrowany na dole
                draw_icon(&mut display, &CLOCK, 50, 75);
                font_big
                    .render_aligned(
                        time_str.as_str(),
                        Point::new(68, 68),
                        u8g2_fonts::types::VerticalPosition::Top,
                        u8g2_fonts::types::HorizontalAlignment::Left,
                        u8g2_fonts::types::FontColor::Transparent(Rgb565::WHITE),
                        &mut display,
                    )
                    .ok();

                // czekamy 2 sekundy ale sprawdzamy przycisk co 50ms
                for _ in 0..40 {
                    delay.delay_millis(50);
                    if btn.is_low() {
                        // czekamy na zwolnienie przycisku i przelaczamy
                        while btn.is_low() {
                            delay.delay_millis(10);
                        }
                        screen = Screen::Sort;
                        break;
                    }
                }

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

                // rysujemy tytul + wszystkie slupki raz
                display.clear(Rgb565::BLACK).ok();
                font_big.render_aligned(
                    "Quick Sort",
                    Point::new(2, 2),
                    u8g2_fonts::types::VerticalPosition::Top,
                    u8g2_fonts::types::HorizontalAlignment::Left,
                    u8g2_fonts::types::FontColor::Transparent(Rgb565::WHITE),
                    &mut display,
                )
                .ok();
                draw_all_bars(&mut display, &arr);

                // quicksort iteracyjny ze stosem o stalym rozmiarze
                let mut stack = [(0usize, 0usize); NUM_BARS];
                let mut stack_top: usize = 0;
                stack[stack_top] = (0, NUM_BARS - 1);
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
                            interrupted = true;
                            break 'sort;
                        }

                        if (arr[j] as usize) <= pivot {
                            if i != j {
                                // faza 1: highlight zrodla (j) przy starej wysokosci
                                draw_bar(&mut display, &arr, j, true);
                                delay.delay_millis(100);

                                // swap
                                arr.swap(i, j);

                                // faza 2: j cyan (nowa wys.), i highlight (nowa wys.)
                                draw_bar(&mut display, &arr, j, false);
                                draw_bar(&mut display, &arr, i, true);
                                delay.delay_millis(100);

                                // faza 3: i wraca do cyan
                                draw_bar(&mut display, &arr, i, false);
                            }
                            // i == j: self-swap jest no-op, pomijamy

                            i += 1;
                        }
                    }

                    // ostatnia zamiana — pivot na swoje miejsce
                    if i != hi {
                        // faza 1: highlight pivota (hi) przy starej wysokosci
                        draw_bar(&mut display, &arr, hi, true);
                        delay.delay_millis(50);

                        // swap
                        arr.swap(i, hi);

                        // faza 2: hi cyan (nowa wys.), i highlight (nowa wys.)
                        draw_bar(&mut display, &arr, hi, false);
                        draw_bar(&mut display, &arr, i, true);
                        delay.delay_millis(50);

                        // faza 3: i wraca do cyan
                        draw_bar(&mut display, &arr, i, false);
                    }
                    // i == hi: pivot juz na miejscu, pomijamy

                    // dodaj podtablice na stos
                    if i > lo && stack_top < NUM_BARS - 1 {
                        stack[stack_top] = (lo, i - 1);
                        stack_top += 1;
                    }
                    if i < hi && stack_top < NUM_BARS - 1 {
                        stack[stack_top] = (i + 1, hi);
                        stack_top += 1;
                    }
                }

                if interrupted {
                    // przycisk wcisniety — czekamy na zwolnienie + cooldown 300ms
                    while btn.is_low() {
                        delay.delay_millis(10);
                    }
                    delay.delay_millis(300);
                    screen = Screen::Weather;
                } else {
                    // sortowanie skonczone — pokaz posortowana tablice chwile, potem losuj od nowa
                    draw_all_bars(&mut display, &arr);
                    delay.delay_millis(500);
                    for k in 0..NUM_BARS {
                        let x = (k * (BAR_WIDTH + BAR_GAP)) as i32;
                        let h = arr[k] as i32;
                        let _ = Rectangle::new(
                            Point::new(x, BAR_TOP),
                            Size::new(BAR_WIDTH as u32, BAR_MAX_H as u32),
                        )
                        .into_styled(PrimitiveStyle::with_fill(Rgb565::BLACK))
                        .draw(&mut display);
                        let _ = Rectangle::new(
                            Point::new(x, BAR_BOTTOM - h),
                            Size::new(BAR_WIDTH as u32, h as u32),
                        )
                        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(0, 63, 0)))
                        .draw(&mut display);
                        delay.delay_millis(80);
                    }
                    delay.delay_millis(3000);
                }
            }
        }
    }
}

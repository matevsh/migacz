#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{delay::Delay, gpio::{Io, Level, Output, OutputOpenDrain, Pull}, prelude::*};
use esp_hal::i2c::I2c;
use sh1106::{prelude::*, Builder};
use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::*,
};
use u8g2_fonts::{fonts, FontRenderer};
use dht_sensor::{dht11, DhtReading};
use core::fmt::Write;
use esp_hal::time::Duration;
use heapless::String;

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

    // LED na GPIO0 - zaswiecimy jak wszystko dziala
    let mut led = Output::new(io.pins.gpio0, Level::Low);

    // pin DHT11 jako open-drain na GPIO3 (pozwala czytac i pisac)
    let mut dht_pin = OutputOpenDrain::new(io.pins.gpio6, Level::High, Pull::Up);
    let mut delay = Delay::new();

    // font z polskimi znakami
    let font = FontRenderer::new::<fonts::u8g2_font_unifont_t_polish>();

    // zegar startowy - ustaw swoja godzine
    let mut hours: u8 = 14;
    let mut minutes: u8 = 30;
    let mut seconds: u8 = 0;

    // licznik do zliczania sekund z opoznien w petli
    let mut tick_counter: u16 = 0;
    // odswiezamy co 2 sekundy
    let refresh_ms: u16 = 2000;

    esp_println::logger::init_logger_from_env();

    loop {
        // odczyt temperatury i wilgotnosci z DHT11
        let (temp, hum) = match dht11::Reading::read(&mut delay, &mut dht_pin) {
            Ok(reading) => {
                led.set_high();
                log::info!("DHT11 OK: temp={}C hum={}%", reading.temperature, reading.relative_humidity);
                (reading.temperature, reading.relative_humidity)
            }
            Err(e) => {
                led.set_low();
                log::error!("DHT11 BLAD: {:?}", e);
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

        // czyszczenie ekranu przed rysowaniem
        display.clear();

        // wiersz 1: termometr + temperatura
        draw_icon(&mut display, &THERMOMETER, 2, 2);
        font.render_aligned(
            temp_str.as_str(),
            Point::new(16, 2),
            u8g2_fonts::types::VerticalPosition::Top,
            u8g2_fonts::types::HorizontalAlignment::Left,
            u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
            &mut display,
        ).ok();

        // wiersz 1: kropla + wilgotnosc (obok temperatury)
        draw_icon(&mut display, &DROP, 66, 2);
        font.render_aligned(
            hum_str.as_str(),
            Point::new(80, 2),
            u8g2_fonts::types::VerticalPosition::Top,
            u8g2_fonts::types::HorizontalAlignment::Left,
            u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
            &mut display,
        ).ok();

        // wiersz 2: zegar + godzina
        draw_icon(&mut display, &CLOCK, 2, 28);
        font.render_aligned(
            time_str.as_str(),
            Point::new(16, 28),
            u8g2_fonts::types::VerticalPosition::Top,
            u8g2_fonts::types::HorizontalAlignment::Left,
            u8g2_fonts::types::FontColor::Transparent(BinaryColor::On),
            &mut display,
        ).ok();

        // wyslanie bufora na wyswietlacz
        display.flush().unwrap();

        // czekamy 2 sekundy przed kolejnym odczytem
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
}

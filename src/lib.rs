#![no_std]

#[cfg(feature = "graphics")]
extern crate embedded_graphics;

use embedded_hal::blocking::delay::DelayMs;
use embedded_hal::digital::v2::OutputPin;

use core::iter::once;
use display_interface::DataFormat::{U16BEIter, U8Iter, U16BE};
use display_interface::WriteOnlyDataCommand;

pub mod spi;

/// Trait representing the interface to the hardware.
///
/// Intended to abstract the various buses (SPI, MPU 8/9/16-bit) from the Controller code.
pub trait Interface {
    type Error;

    /// Sends a command with a sequence of 8-bit arguments
    ///
    /// Mostly used for sending configuration commands
    fn write(&mut self, command: u8, data: &[u8]) -> Result<(), Self::Error>;

    /// Sends a command with a sequence of 16-bit data words
    ///
    /// Mostly used for sending MemoryWrite command and other commands
    /// with 16-bit arguments
    fn write_iter(
        &mut self,
        command: u8,
        data: impl IntoIterator<Item = u16>,
    ) -> Result<(), Self::Error>;
}

const WIDTH: usize = 240;
const HEIGHT: usize = 320;

#[derive(Debug)]
pub enum Error<PinE> {
    Interface,
    OutputPin(PinE),
}

/// The default orientation is Portrait
pub enum Orientation {
    Portrait,
    PortraitFlipped,
    Landscape,
    LandscapeFlipped,
}

/// There are two method for drawing to the screen:
/// [draw_raw](struct.Ili9341.html#method.draw_raw) and
/// [draw_iter](struct.Ili9341.html#method.draw_iter).
///
/// In both cases the expected pixel format is rgb565.
///
/// The hardware makes it efficient to draw rectangles on the screen.
///
/// What happens is the following:
///
/// - A drawing window is prepared (with the 2 opposite corner coordinates)
/// - The starting point for drawint is the top left corner of this window
/// - Every pair of bytes received is intepreted as a pixel value in rgb565
/// - As soon as a pixel is received, an internal counter is incremented,
///   and the next word will fill the next pixel (the adjacent on the right, or
///   the first of the next row if the row ended)
pub struct Ili9341<IFACE, RESET> {
    interface: IFACE,
    reset: RESET,
    width: usize,
    height: usize,
}

impl<PinE, IFACE, RESET> Ili9341<IFACE, RESET>
where
    IFACE: WriteOnlyDataCommand,
    RESET: OutputPin<Error = PinE>,
{
    pub fn new<DELAY: DelayMs<u16>>(
        interface: IFACE,
        reset: RESET,
        delay: &mut DELAY,
    ) -> Result<Self, Error<PinE>> {
        let mut ili9341 = Ili9341 {
            interface,
            reset,
            width: WIDTH,
            height: HEIGHT,
        };

        // Do hardware reset by holding reset low for at least 10us
        ili9341.reset.set_low().map_err(Error::OutputPin)?;
        delay.delay_ms(1);
        // Set high for normal operation
        ili9341.reset.set_high().map_err(Error::OutputPin)?;

        // Wait 5ms after reset before sending commands
        // and 120ms before sending Sleep Out
        delay.delay_ms(5);

        // Do software reset
        ili9341.command(Command::SoftwareReset, &[])?;

        // Wait 5ms after reset before sending commands
        // and 120ms before sending Sleep Out
        delay.delay_ms(120);

        ili9341.set_orientation(Orientation::Portrait)?;

        // Set pixel format to 16 bits per pixel
        ili9341.command(Command::PixelFormatSet, &[0x55])?;

        ili9341.command(Command::SleepOut, &[])?;

        // Wait 5ms after Sleep Out before sending commands
        delay.delay_ms(5);

        ili9341.command(Command::DisplayOn, &[])?;

        Ok(ili9341)
    }

    fn command(&mut self, cmd: Command, args: &[u8]) -> Result<(), Error<PinE>> {
        self.interface
            .send_commands(U8Iter(&mut once(cmd as u8)))
            .map_err(|_| Error::Interface)?;
        self.interface
            .send_data(U8Iter(&mut args.iter().cloned()))
            .map_err(|_| Error::Interface)
    }

    fn write(&mut self, data: &mut [u16]) -> Result<(), Error<PinE>> {
        self.command(Command::MemoryWrite, &[])?;
        self.interface
            .send_data(U16BE(data))
            .map_err(|_| Error::Interface)
    }

    fn write_iter<I: IntoIterator<Item = u16>>(&mut self, data: I) -> Result<(), Error<PinE>> {
        self.command(Command::MemoryWrite, &[])?;
        self.interface
            .send_data(U16BEIter(&mut data.into_iter()))
            .map_err(|_| Error::Interface)
    }

    fn set_window(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) -> Result<(), Error<PinE>> {
        self.command(
            Command::ColumnAddressSet,
            &[
                (x0 >> 8) as u8,
                (x0 & 0xff) as u8,
                (x1 >> 8) as u8,
                (x1 & 0xff) as u8,
            ],
        )?;
        self.command(
            Command::PageAddressSet,
            &[
                (y0 >> 8) as u8,
                (y0 & 0xff) as u8,
                (y1 >> 8) as u8,
                (y1 & 0xff) as u8,
            ],
        )?;
        Ok(())
    }

    /// Configures the screen for hardware-accelerated vertical scrolling.
    pub fn configure_vertical_scroll(
        &mut self,
        fixed_top_lines: u16,
        fixed_bottom_lines: u16,
    ) -> Result<Scroller, Error<PinE>> {
        let scroll_lines = HEIGHT as u16 - fixed_top_lines - fixed_bottom_lines;

        self.command(
            Command::VerticalScrollDefine,
            &[
                (fixed_top_lines >> 8) as u8,
                (fixed_top_lines & 0xff) as u8,
                (scroll_lines >> 8) as u8,
                (scroll_lines & 0xff) as u8,
                (fixed_bottom_lines >> 8) as u8,
                (fixed_bottom_lines & 0xff) as u8,
            ],
        )?;

        Ok(Scroller::new(fixed_top_lines, fixed_bottom_lines))
    }

    pub fn scroll_vertically(
        &mut self,
        scroller: &mut Scroller,
        num_lines: u16,
    ) -> Result<(), Error<PinE>> {
        let height = HEIGHT as u16;
        scroller.top_offset += num_lines;
        if scroller.top_offset > (height - scroller.fixed_bottom_lines) {
            scroller.top_offset = scroller.fixed_top_lines
                + (scroller.top_offset - height + scroller.fixed_bottom_lines)
        }

        self.command(
            Command::VerticalScrollAddr,
            &[
                (scroller.top_offset >> 8) as u8,
                (scroller.top_offset & 0xff) as u8,
            ],
        )
    }

    /// Draw a rectangle on the screen, represented by top-left corner (x0, y0)
    /// and bottom-right corner (x1, y1).
    ///
    /// The border is included.
    ///
    /// This method accepts an iterator of rgb565 pixel values.
    ///
    /// The iterator is useful to avoid wasting memory by holding a buffer for
    /// the whole screen when it is not necessary.
    pub fn draw_iter<I: IntoIterator<Item = u16>>(
        &mut self,
        x0: u16,
        y0: u16,
        x1: u16,
        y1: u16,
        data: I,
    ) -> Result<(), Error<PinE>> {
        self.set_window(x0, y0, x1, y1)?;
        self.write_iter(data)
    }

    /// Draw a rectangle on the screen, represented by top-left corner (x0, y0)
    /// and bottom-right corner (x1, y1).
    ///
    /// The border is included.
    ///
    /// This method accepts a raw buffer of words that will be copied to the screen
    /// video memory.
    ///
    /// The expected format is rgb565.
    pub fn draw_raw(
        &mut self,
        x0: u16,
        y0: u16,
        x1: u16,
        y1: u16,
        data: &[u16],
    ) -> Result<(), Error<PinE>> {
        self.set_window(x0, y0, x1, y1)?;
        self.write_iter(data.iter().cloned())
    }

    /// Draw a rectangle on the screen, represented by top-left corner (x0, y0)
    /// and bottom-right corner (x1, y1).
    ///
    /// The border is included.
    ///
    /// This method accepts a raw buffer of words that will be copied to the screen
    /// video memory.
    ///
    /// The expected format is rgb565.
    pub fn draw_raw_non_iter(
        &mut self,
        x0: u16,
        y0: u16,
        x1: u16,
        y1: u16,
        data: &mut [u16],
    ) -> Result<(), Error<PinE>> {
        self.set_window(x0, y0, x1, y1)?;
        self.write(data)
    }

    /// Change the orientation of the screen
    pub fn set_orientation(&mut self, mode: Orientation) -> Result<(), Error<PinE>> {
        match mode {
            Orientation::Portrait => {
                self.width = WIDTH;
                self.height = HEIGHT;
                self.command(Command::MemoryAccessControl, &[0x40 | 0x08])
            }
            Orientation::Landscape => {
                self.width = HEIGHT;
                self.height = WIDTH;
                self.command(Command::MemoryAccessControl, &[0x20 | 0x08])
            }
            Orientation::PortraitFlipped => {
                self.width = WIDTH;
                self.height = HEIGHT;
                self.command(Command::MemoryAccessControl, &[0x80 | 0x08])
            }
            Orientation::LandscapeFlipped => {
                self.width = HEIGHT;
                self.height = WIDTH;
                self.command(Command::MemoryAccessControl, &[0x40 | 0x80 | 0x20 | 0x08])
            }
        }
    }

    pub fn sleep_in(&mut self) -> Result<(), Error<PinE>> {
        self.command(Command::SleepIn, &[])
    }

    pub fn sleep_out(&mut self) -> Result<(), Error<PinE>> {
        self.command(Command::SleepOut, &[])
    }

    /// Get the current screen width. It can change based on the current orientation
    pub fn width(&self) -> usize {
        self.width
    }

    /// Get the current screen heighth. It can change based on the current orientation
    pub fn height(&self) -> usize {
        self.height
    }
}

/// Scroller must be provided in order to scroll the screen. It can only be obtained
/// by configuring the screen for scrolling.
pub struct Scroller {
    top_offset: u16,
    fixed_bottom_lines: u16,
    fixed_top_lines: u16,
}

impl Scroller {
    fn new(fixed_top_lines: u16, fixed_bottom_lines: u16) -> Scroller {
        Scroller {
            top_offset: fixed_top_lines,
            fixed_top_lines,
            fixed_bottom_lines,
        }
    }
}

#[cfg(feature = "graphics")]
mod graphics;

#[derive(Clone, Copy)]
enum Command {
    SoftwareReset = 0x01,
    MemoryAccessControl = 0x36,
    PixelFormatSet = 0x3a,
    SleepIn = 0x10,
    SleepOut = 0x11,
    DisplayOn = 0x29,
    ColumnAddressSet = 0x2a,
    PageAddressSet = 0x2b,
    MemoryWrite = 0x2c,
    VerticalScrollDefine = 0x33,
    VerticalScrollAddr = 0x37,
}

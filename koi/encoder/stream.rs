use super::writer::Writer;
use crate::{
    types::{color_diff, luma_diff, Channels, Op, RgbaColor, CACHE_SIZE, END_OF_IMAGE},
    util::pixel_hash,
};
use lz4_flex::frame::FrameEncoder;
use std::io::{self, BufWriter, Read, Write};

// PixelEncoder is a stream encoder that encodes pixels one by one
// - Writer is a wrapper around the underlying writer that can be either a lz4 encoder or a regular writer
// - C is the number of channels in the image
pub struct PixelEncoder<W: Write, const C: usize> {
    writer: Writer<W>,
    // runlength: u8,    // if runlength > 0 then we are in runlength encoding mode
    pixels_in: usize, // pixels encoded so far
    pixels_count: usize,

    cache: [RgbaColor; CACHE_SIZE],
    prev_pixel: RgbaColor,

    buffer: Vec<u8>,
}

impl<W: Write, const C: usize> PixelEncoder<W, C> {
    pub fn new(writer: Writer<W>, pixels_count: usize) -> Self {
        Self {
            writer,
            cache: [RgbaColor([0, 0, 0, 0]); CACHE_SIZE],
            // runlength: 0,
            pixels_in: 0,
            pixels_count,
            prev_pixel: RgbaColor([0, 0, 0, 0]),

            buffer: Vec::with_capacity(8),
        }
    }

    pub fn new_lz4(writer: W, pixels_count: usize) -> Self {
        Self::new(
            Writer::Lz4Encoder(Box::new(FrameEncoder::new(writer))),
            pixels_count,
        )
    }

    pub fn new_uncompressed(writer: W, pixels_count: usize) -> Self {
        Self::new(
            Writer::UncompressedEncoder(BufWriter::new(writer)),
            pixels_count,
        )
    }

    #[inline]
    fn encode_pixel(
        &mut self,
        curr_pixel: RgbaColor,
        prev_pixel: RgbaColor,
    ) -> std::io::Result<()> {
        if C < Channels::Rgba as u8 as usize {
            // alpha channel should be 255 for all pixels in RGB images
            assert_eq!(
                curr_pixel.0[3], 255,
                "alpha channel should be 255 for all pixels in RGB images"
            );
        };

        self.pixels_in += 1;
        let mut curr_pixel = curr_pixel;

        // index encoding
        let hash = pixel_hash(curr_pixel);
        if self.cache[hash as usize] == curr_pixel {
            self.cache_pixel(&mut curr_pixel);
            self.writer.write_one(u8::from(Op::Index) | hash)?;
            return Ok(());
        }

        // alpha diff encoding (whenever only alpha channel changes)
        if curr_pixel.0[..3] == prev_pixel.0[..3] && curr_pixel.0[3] != prev_pixel.0[3] {
            if let Some(diff) = prev_pixel.alpha_diff(&curr_pixel) {
                self.cache_pixel(&mut curr_pixel);
                self.writer.write_one(diff)?;
                return Ok(());
            }
        }

        let is_gray = curr_pixel.is_gray();
        if curr_pixel.0[3] != prev_pixel.0[3] && curr_pixel.0[3] != 255 {
            if is_gray {
                // Gray Alpha encoding (whenever alpha channel changes and pixel is gray)
                self.cache_pixel(&mut curr_pixel);
                self.writer.write_one(Op::GrayAlpha as u8)?;
                self.writer.write_all(&[curr_pixel.0[0], curr_pixel.0[3]])?;
                return Ok(());
            } else {
                // RGBA encoding (whenever alpha channel changes)
                self.cache_pixel(&mut curr_pixel);
                self.writer.write_one(Op::Rgba as u8)?;
                self.writer.write_all(&curr_pixel.0)?;
                return Ok(());
            }
        }

        // Difference between current and previous pixel
        let diff = curr_pixel.diff(&prev_pixel);

        // Diff encoding
        if let Some(diff) = color_diff(diff) {
            self.cache_pixel(&mut curr_pixel);
            self.writer.write_one(diff)?;
            return Ok(());
        }

        // Luma encoding
        if let Some(luma) = luma_diff(diff) {
            self.cache_pixel(&mut curr_pixel);
            self.writer.write_all(&luma)?;
            return Ok(());
        }

        if is_gray {
            // Gray encoding
            let RgbaColor([r, g, b, _]) = curr_pixel;
            if r == g && g == b {
                self.cache_pixel(&mut curr_pixel);
                self.writer.write_one(Op::Gray as u8)?;
                self.writer.write_one(curr_pixel.0[0])?;
                return Ok(());
            }
        }

        // RGB encoding
        let RgbaColor([r, g, b, _]) = curr_pixel;
        self.cache_pixel(&mut curr_pixel);
        self.writer.write_all(&[Op::Rgb as u8, r, g, b])?;
        Ok(())
    }

    #[inline]
    fn cache_pixel(&mut self, curr_pixel: &mut RgbaColor) {
        let hash = pixel_hash(*curr_pixel);
        self.cache[hash as usize] = *curr_pixel;
    }

    // flushes the remaining pixels in the cache and writes the end of image marker, automatically called after N pixels are encoded
    pub fn finish(&mut self) -> std::io::Result<()> {
        self.writer.write_all(&END_OF_IMAGE)
    }

    // take a reader and encode it pixel by pixel
    pub fn encode<R: Read>(&mut self, mut reader: R) -> std::io::Result<u64> {
        io::copy(&mut reader, self)
    }
}

impl<W: Write, const C: usize> Write for PixelEncoder<W, C> {
    // Currently always buffers C bytes before encoding a pixel, this could be improved by only buffering the remaining bytes until the next pixel boundary is reached
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let buf_len = buf.len();
        // let mut total_bytes_written = 0;

        for byte in buf {
            self.buffer.push(*byte);
            if self.buffer.len() == C {
                let mut curr_pixel = RgbaColor([0, 0, 0, 255]);
                curr_pixel.0[..C].copy_from_slice(&self.buffer);
                self.encode_pixel(curr_pixel, self.prev_pixel)?;
                self.prev_pixel = curr_pixel;
                self.buffer.clear();
                // total_bytes_written += C;
            }
        }

        if self.pixels_in == self.pixels_count {
            self.finish()?;
        }

        Ok(buf_len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;

        if !self.buffer.is_empty() {
            println!("buffer not empty, are the amount of channels correct?");
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "buffer not empty",
            ))
        } else {
            Ok(())
        }
    }
}

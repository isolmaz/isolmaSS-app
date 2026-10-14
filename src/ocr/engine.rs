//! The Windows.Media.Ocr engine behind [`Engine`]. Only a Windows install with a text
//! recognition language can run it, so it stays thin: the pixels go in as a BGRA8
//! `SoftwareBitmap` and the words come out as plain data. The logic around it is in `ocr.rs`.
use super::{Engine, OcrFailure, OcrImage, OcrLine, OcrWord, Recognized, WordBox};
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::{OcrEngine, OcrResult};
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// The engine for the languages of the user profile.
pub struct SystemEngine;

/// The limit `OcrEngine::MaxImageDimension` reports on current Windows, used when the
/// query fails.
const FALLBACK_MAX_DIMENSION: u32 = 2600;

impl Engine for SystemEngine {
    fn max_dimension(&self) -> u32 {
        let _apartment = Apartment::enter();
        OcrEngine::MaxImageDimension().unwrap_or(FALLBACK_MAX_DIMENSION)
    }

    fn recognize(&self, image: &OcrImage) -> Recognized {
        let _apartment = Apartment::enter();
        // Without a recognition language Windows hands back no engine at all, which the
        // bindings report as an error without an error code.
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|error| {
            if error.code().is_ok() {
                OcrFailure::NoLanguage
            } else {
                failed(error)
            }
        })?;
        let bitmap = bitmap_of(image).map_err(failed)?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .and_then(|operation| operation.get())
            .map_err(failed)?;
        lines_of(&result).map_err(failed)
    }
}

fn failed(error: windows::core::Error) -> OcrFailure {
    OcrFailure::Failed(error.to_string())
}

/// The calling thread, a worker without COM, joins the multithreaded apartment while a
/// recognition uses Windows Runtime objects.
struct Apartment(bool);

impl Apartment {
    fn enter() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn bitmap_of(image: &OcrImage) -> windows::core::Result<SoftwareBitmap> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(&image.bgra)?;
    SoftwareBitmap::CreateCopyFromBuffer(
        &writer.DetachBuffer()?,
        BitmapPixelFormat::Bgra8,
        image.width,
        image.height,
    )
}

fn lines_of(result: &OcrResult) -> windows::core::Result<Vec<OcrLine>> {
    let mut lines = Vec::new();
    for line in result.Lines()? {
        let mut words = Vec::new();
        for word in line.Words()? {
            let rect = word.BoundingRect()?;
            words.push(OcrWord {
                text: word.Text()?.to_string(),
                bounds: WordBox::new(rect.X, rect.Y, rect.X + rect.Width, rect.Y + rect.Height),
            });
        }
        lines.push(OcrLine {
            text: line.Text()?.to_string(),
            words,
        });
    }
    Ok(lines)
}

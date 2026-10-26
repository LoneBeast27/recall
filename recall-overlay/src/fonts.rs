//! DirectWrite text for the island: Geist (500/600) and Bricolage Grotesque
//! 700 loaded from memory (IDWriteFactory5 in-memory loader, never installed),
//! tabular figures on every run, and [`Measure`] for the layout.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, Interface, Result, PCWSTR};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory5, IDWriteFontCollection, IDWriteFontCollection1,
    IDWriteInMemoryFontFileLoader, IDWriteTextFormat, IDWriteTextLayout, IDWriteTypography,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
    DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP,
};

use crate::layout::{Face, Measure, Role, TextMetrics};

const GEIST_500: &[u8] = include_bytes!("../../recall-native/assets/fonts/geist-500.ttf");
const GEIST_600: &[u8] = include_bytes!("../../recall-native/assets/fonts/geist-600.ttf");
const BRICOLAGE_700: &[u8] = include_bytes!("../../recall-native/assets/fonts/bricolage-grotesque-700.ttf");

struct Family {
    collection: IDWriteFontCollection,
    /// The family name DirectWrite derived for the font(s) in this collection.
    name: Vec<u16>,
    /// Cap height / em.
    cap: f32,
}

pub struct Fonts {
    factory: IDWriteFactory5,
    _loader: IDWriteInMemoryFontFileLoader,
    geist: Family,
    bricolage: Family,
    tabular: IDWriteTypography,
    formats: RefCell<HashMap<(Role, u32), IDWriteTextFormat>>,
}

impl Fonts {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let loader = factory.CreateInMemoryFontFileLoader()?;
            factory.RegisterFontFileLoader(&loader)?;
            let geist = family(&factory, &loader, &[GEIST_500, GEIST_600], 500)?;
            let bricolage = family(&factory, &loader, &[BRICOLAGE_700], 700)?;
            let tabular = factory.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
            Ok(Self { factory, _loader: loader, geist, bricolage, tabular, formats: RefCell::default() })
        }
    }

    /// (UI family, display family) as DirectWrite resolved them.
    pub fn family_names(&self) -> (String, String) {
        let name = |f: &Family| String::from_utf16_lossy(&f.name[..f.name.len().saturating_sub(1)]);
        (name(&self.geist), name(&self.bricolage))
    }

    fn family(&self, face: Face) -> (&Family, i32) {
        match face {
            Face::Geist500 => (&self.geist, 500),
            Face::Geist600 => (&self.geist, 600),
            Face::Bricolage700 => (&self.bricolage, 700),
        }
    }

    fn format(&self, role: Role, scale: f32) -> Result<IDWriteTextFormat> {
        let key = (role, (scale * 1000.0).round() as u32);
        if let Some(format) = self.formats.borrow().get(&key) {
            return Ok(format.clone());
        }
        let (family, weight) = self.family(role.face());
        let format = unsafe {
            let format = self.factory.CreateTextFormat(
                PCWSTR(family.name.as_ptr()),
                &family.collection,
                DWRITE_FONT_WEIGHT(weight),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                role.size() * scale,
                w!("en-us"),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            format
        };
        self.formats.borrow_mut().insert(key, format.clone());
        Ok(format)
    }

    /// A single-line layout with tabular figures.
    pub fn layout(&self, role: Role, scale: f32, text: &str) -> Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = self.factory.CreateTextLayout(&wide, &self.format(role, scale)?, 4096.0, 1024.0)?;
            layout.SetTypography(&self.tabular, DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 })?;
            Ok(layout)
        }
    }

    fn metrics(&self, role: Role, scale: f32, text: &str) -> Result<TextMetrics> {
        let layout = self.layout(role, scale, text)?;
        let mut text_metrics = DWRITE_TEXT_METRICS::default();
        let mut lines = [DWRITE_LINE_METRICS::default()];
        let mut count = 0u32;
        unsafe {
            layout.GetMetrics(&mut text_metrics)?;
            layout.GetLineMetrics(Some(&mut lines), &mut count)?;
        }
        let (family, _) = self.family(role.face());
        Ok(TextMetrics {
            width: text_metrics.widthIncludingTrailingWhitespace,
            height: text_metrics.height,
            baseline: lines[0].baseline,
            cap: family.cap * role.size() * scale,
        })
    }
}

impl Measure for Fonts {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
        self.metrics(role, scale, text).unwrap_or_default()
    }
}

unsafe fn family(
    factory: &IDWriteFactory5,
    loader: &IDWriteInMemoryFontFileLoader,
    files: &[&'static [u8]],
    weight: i32,
) -> Result<Family> {
    unsafe {
        let builder = factory.CreateFontSetBuilder()?;
        for bytes in files {
            let file = loader.CreateInMemoryFontFileReference(
                factory,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                None::<&windows::core::IUnknown>,
            )?;
            builder.AddFontFile(&file)?;
        }
        let set = builder.CreateFontSet()?;
        let collection: IDWriteFontCollection1 = factory.CreateFontCollectionFromFontSet(&set)?;
        let collection: IDWriteFontCollection = collection.cast()?;
        let first = collection.GetFontFamily(0)?;
        let names = first.GetFamilyNames()?;
        let len = names.GetStringLength(0)? as usize;
        let mut name = vec![0u16; len + 1];
        names.GetString(0, &mut name)?;
        let font = first.GetFirstMatchingFont(DWRITE_FONT_WEIGHT(weight), DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)?;
        let mut metrics = DWRITE_FONT_METRICS::default();
        font.GetMetrics(&mut metrics);
        let cap = metrics.capHeight as f32 / metrics.designUnitsPerEm.max(1) as f32;
        Ok(Family { collection, name, cap })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_fonts_load_from_memory_with_their_own_families() {
        let fonts = Fonts::new().expect("DirectWrite in-memory fonts");
        let (ui, display) = fonts.family_names();
        assert_eq!(ui, "Geist");
        assert!(display.starts_with("Bricolage Grotesque"), "{display}");
        let metrics = fonts.measure(Role::Fps, 1.0, "144");
        assert!(metrics.width > 10.0 && metrics.width < 60.0, "{metrics:?}");
        assert!(metrics.cap > 8.0 && metrics.cap < 18.0 && metrics.baseline > metrics.cap, "{metrics:?}");
    }

    /// Width of `text` with a family that is not in `collection`: DirectWrite's system fallback.
    fn fallback_width(fonts: &Fonts, collection: &IDWriteFontCollection, weight: i32, size: f32, text: &str) -> f32 {
        let name: Vec<u16> = "No Such Family\0".encode_utf16().collect();
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            let format = fonts
                .factory
                .CreateTextFormat(
                    PCWSTR(name.as_ptr()),
                    collection,
                    DWRITE_FONT_WEIGHT(weight),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
                .unwrap();
            let layout = fonts.factory.CreateTextLayout(&wide, &format, 4096.0, 1024.0).unwrap();
            layout.GetMetrics(&mut metrics).unwrap();
        }
        metrics.widthIncludingTrailingWhitespace
    }

    #[test]
    fn faces_resolve_to_the_embedded_fonts_not_a_system_fallback() {
        let fonts = Fonts::new().unwrap();
        let text = "Rocket League";
        let bricolage = fonts.measure(Role::NoticeGame, 1.0, text).width;
        let fallback = fallback_width(&fonts, &fonts.bricolage.collection, 700, Role::NoticeGame.size(), text);
        assert!((bricolage - fallback).abs() > 0.5, "Bricolage {bricolage} vs fallback {fallback}");
        let geist = fonts.measure(Role::NoticeUi, 1.0, text).width;
        let fallback = fallback_width(&fonts, &fonts.geist.collection, 600, Role::NoticeUi.size(), text);
        assert!((geist - fallback).abs() > 0.5, "Geist {geist} vs fallback {fallback}");
    }

    #[test]
    fn tabular_figures_give_every_digit_the_same_advance() {
        let fonts = Fonts::new().unwrap();
        let ones = fonts.measure(Role::Fps, 1.0, "111").width;
        let eights = fonts.measure(Role::Fps, 1.0, "888").width;
        assert!((ones - eights).abs() < 0.01, "111 = {ones}, 888 = {eights}");
    }
}

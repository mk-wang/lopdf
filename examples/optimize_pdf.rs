use lopdf::{Document, LoadOptions, OptimizeOptions, SaveOptions};
use std::error::Error;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[cfg(feature = "async")]
use tokio::runtime::Builder;

#[cfg(not(feature = "async"))]
fn load_document(path: &Path) -> Result<Document, Box<dyn Error>> {
    Ok(Document::load_with_options(
        path,
        LoadOptions::with_max_decompressed_size(512 * 1024 * 1024),
    )?)
}

#[cfg(feature = "async")]
fn load_document(path: &Path) -> Result<Document, Box<dyn Error>> {
    Ok(Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(Document::load_with_options(
            path,
            LoadOptions::with_max_decompressed_size(512 * 1024 * 1024),
        ))?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let input = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: optimize_pdf INPUT.pdf [OUTPUT.pdf]")?;
    let output = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_output_path(&input));
    if arguments.next().is_some() {
        return Err("usage: optimize_pdf INPUT.pdf [OUTPUT.pdf]".into());
    }
    if input == output {
        return Err("the output path must not overwrite the input PDF".into());
    }

    let original_size = std::fs::metadata(&input)?.len();
    let started = Instant::now();
    let mut document = load_document(&input)?;
    let page_count = document.get_pages().len();
    let report = document.optimize(&OptimizeOptions::maximum_compression())?;

    let mut output_file = File::create(&output)?;
    let save_options = SaveOptions::builder()
        .use_object_streams(true)
        .use_xref_streams(true)
        .max_objects_per_stream(200)
        .compression_level(9)
        .build();
    document.save_with_options(&mut output_file, save_options)?;
    drop(output_file);

    let optimized_size = std::fs::metadata(&output)?.len();
    let reduction = 100.0 - optimized_size as f64 / original_size as f64 * 100.0;
    println!("input: {}", input.display());
    println!("output: {}", output.display());
    println!("pages: {page_count}");
    println!("original bytes: {original_size}");
    println!("optimized bytes: {optimized_size}");
    println!("reduction: {reduction:.2}%");
    println!("elapsed: {:.3}s", started.elapsed().as_secs_f64());
    println!("structure: {:#?}", report.structure);
    println!("fonts: {:#?}", report.fonts);
    println!("images: {:#?}", report.images);
    Ok(())
}

fn default_output_path(input: &Path) -> PathBuf {
    let mut output = input.to_path_buf();
    let stem = input
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    output.set_file_name(format!("{stem}.optimized.pdf"));
    output
}

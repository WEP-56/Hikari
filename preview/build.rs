use std::env;
use std::fs::File;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let png = manifest.join("../icon.png");
    println!("cargo:rerun-if-changed={}", png.display());

    let ico_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("hikari.ico");
    write_ico(&png, &ico_path);

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_str().expect("icon path"));
    res.compile().expect("embed window icon");
}

fn write_ico(png: &std::path::Path, dest: &std::path::Path) {
    let source = image::open(png).expect("read icon.png").into_rgba8();
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 24, 32, 48, 64, 128, 256] {
        let resized =
            image::imageops::resize(&source, size, size, image::imageops::FilterType::Lanczos3);
        let image = ico::IconImage::from_rgba_data(size, size, resized.into_raw());
        dir.add_entry(ico::IconDirEntry::encode(&image).expect("encode icon size"));
    }
    let mut file = File::create(dest).expect("create ico");
    dir.write(&mut file).expect("write ico");
}

use std::path::Path;
fn main() {
    let p = Path::new("/home/matteo/Music/18K, 4997, Masoo - Yemen (Live documentary).mp3");
    let t = turntable::meta::load_meta(p);
    println!("titolo: {}", t.title);
    println!("durata: {}", t.duration);
    println!("art: {} byte", t.art.as_ref().map(|a| a.len()).unwrap_or(0));
    // write_art_to_file
    match turntable::meta::write_art_to_file(&t) {
        Some(u) => println!("artUrl: {}", u),
        None => println!("artUrl: None"),
    }
}

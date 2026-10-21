//! Exercises a Windows cell from inside: reads from the layers, writes
//! that must land in the upper, deletes that must hide lower files,
//! renames, directory listings, memory mapping and DLL loading, and a
//! child process that must see the same root. Prints one line per check
//! and exits non-zero when any check fails.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::Security::{GetTokenInformation, TokenIsAppContainer, TOKEN_QUERY};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, PAGE_READONLY,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_READ, REG_DWORD, REG_EXPAND_SZ,
    REG_SZ,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct T {
    failed: u32,
    passed: u32,
}

impl T {
    fn check(&mut self, name: &str, ok: bool, detail: impl FnOnce() -> String) {
        if ok {
            self.passed += 1;
            println!("PASS {name}");
        } else {
            self.failed += 1;
            println!("FAIL {name}: {}", detail());
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A registry value under HKCU as (type, bytes), or the error code.
fn reg_get(subkey: &str, name: &str) -> Result<(u32, Vec<u8>), u32> {
    let k = wide(subkey);
    let n = wide(name);
    unsafe {
        let mut h: HKEY = std::ptr::null_mut();
        let rc = RegOpenKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, KEY_READ, &mut h);
        if rc != 0 {
            return Err(rc);
        }
        let mut kind = 0u32;
        let mut len = 0u32;
        let rc = RegQueryValueExW(h, n.as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut len);
        if rc != 0 {
            RegCloseKey(h);
            return Err(rc);
        }
        let mut buf = vec![0u8; len as usize];
        let rc = RegQueryValueExW(h, n.as_ptr(), std::ptr::null(), &mut kind, buf.as_mut_ptr(), &mut len);
        RegCloseKey(h);
        if rc != 0 {
            return Err(rc);
        }
        buf.truncate(len as usize);
        Ok((kind, buf))
    }
}

fn reg_string(subkey: &str, name: &str) -> Result<(u32, String), u32> {
    let (kind, bytes) = reg_get(subkey, name)?;
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    Ok((kind, String::from_utf16_lossy(&units[..end])))
}

fn reg_dword(subkey: &str, name: &str) -> Result<u32, u32> {
    let (kind, bytes) = reg_get(subkey, name)?;
    if kind != REG_DWORD || bytes.len() != 4 {
        return Err(u32::MAX);
    }
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Create the key and set a REG_DWORD; the error code on failure.
fn reg_set_dword(subkey: &str, name: &str, v: u32) -> Result<(), u32> {
    let k = wide(subkey);
    let n = wide(name);
    unsafe {
        let mut h: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_ALL_ACCESS,
            std::ptr::null(),
            &mut h,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            return Err(rc);
        }
        let b = v.to_le_bytes();
        let rc = RegSetValueExW(h, n.as_ptr(), 0, REG_DWORD, b.as_ptr(), 4);
        RegCloseKey(h);
        if rc != 0 {
            return Err(rc);
        }
        Ok(())
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn main() {
    let mut t = T {
        failed: 0,
        passed: 0,
    };
    let root = PathBuf::from(std::env::var("CELL_ROOT").unwrap_or_default());
    println!("root: {}", root.display());
    println!(
        "cwd: {}",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    println!("args: {:?}", std::env::args().skip(1).collect::<Vec<_>>());
    println!(
        "USERPROFILE: {}",
        std::env::var("USERPROFILE").unwrap_or_default()
    );

    let app = root.join("app");
    let data = app.join("data");

    // 1. Read a file from the app layer.
    let hello = fs::read_to_string(data.join("hello.txt"));
    t.check(
        "read lower file",
        hello.as_deref().ok().map(|s| s.trim()) == Some("hello from the layer"),
        || format!("{hello:?}"),
    );

    // 2. Case-insensitive lookup.
    let upper = fs::read_to_string(data.join("HELLO.TXT"));
    t.check("case-insensitive lookup", upper.is_ok(), || {
        format!("{upper:?}")
    });

    // 3. Listing of a lower directory. `gone.txt` is absent after an
    // earlier run deleted it, so it is not required.
    let listed = names(&data);
    let expected = ["big.bin", "hello.txt", "sub"];
    let ok = expected.iter().all(|e| listed.iter().any(|n| n == e))
        && listed
            .iter()
            .all(|n| expected.contains(&n.as_str()) || n == "gone.txt");
    t.check("list lower dir", ok, || format!("{listed:?}"));

    // 4. A big file reads back as the pattern the build wrote.
    let big = fs::read(data.join("big.bin")).unwrap_or_default();
    let ok = big.len() == 16 << 20
        && big
            .chunks(4)
            .enumerate()
            .all(|(i, c)| c == (i as u32).to_le_bytes());
    t.check("read 16 MiB pattern", ok, || format!("len {}", big.len()));

    // 5. Partial read at an offset.
    let mut f = fs::File::open(data.join("big.bin")).unwrap();
    use std::io::Seek;
    f.seek(std::io::SeekFrom::Start(8 << 20)).unwrap();
    let mut four = [0u8; 4];
    f.read_exact(&mut four).unwrap();
    t.check(
        "pread at 8 MiB",
        u32::from_le_bytes(four) == (8 << 20) / 4,
        || format!("{four:?}"),
    );

    // 6. Write a new file (goes to the upper), read it back.
    let newf = data.join("new.txt");
    let _ = fs::remove_file(&newf);
    let w = fs::write(&newf, "written in the sandbox");
    let back = fs::read_to_string(&newf);
    t.check(
        "create and read back",
        w.is_ok() && back.as_deref().ok() == Some("written in the sandbox"),
        || format!("{w:?} {back:?}"),
    );

    // 7. Modify a lower file: copy-up, then the change is visible.
    let edit = data.join("hello.txt");
    let a = fs::OpenOptions::new()
        .append(true)
        .open(&edit)
        .and_then(|mut f| f.write_all("\nappended".as_bytes()));
    let after = fs::read_to_string(&edit).unwrap_or_default();
    t.check(
        "append to lower file (copy-up)",
        a.is_ok() && after.contains("appended") && after.starts_with("hello"),
        || format!("{a:?} {after:?}"),
    );
    // Put it back so the test is repeatable from the same upper.
    let _ = fs::write(&edit, "hello from the layer\n");

    // 8. Delete a lower file: it must vanish from lookups and listings.
    let gone = data.join("gone.txt");
    let _ = fs::write(&gone, "x");
    let d = fs::remove_file(&gone);
    let still = gone.exists();
    let listed = names(&data);
    t.check(
        "delete lower file (whiteout)",
        d.is_ok() && !still && !listed.iter().any(|n| n == "gone.txt"),
        || format!("{d:?} exists={still} {listed:?}"),
    );
    let _ = fs::remove_file(&newf);

    // 9. Make a directory, rename a file into it, list it.
    let mk = data.join("made");
    let _ = fs::remove_dir_all(&mk);
    let m = fs::create_dir(&mk);
    let _ = fs::write(data.join("tmp.txt"), "move me");
    let r = fs::rename(data.join("tmp.txt"), mk.join("moved.txt"));
    let moved = fs::read_to_string(mk.join("moved.txt"));
    t.check(
        "mkdir and rename",
        m.is_ok() && r.is_ok() && moved.as_deref().ok() == Some("move me"),
        || format!("{m:?} {r:?} {moved:?}"),
    );
    let _ = fs::remove_dir_all(&mk);

    // 10. Rename a lower file (copy-up then move, old name whited out).
    let src = data.join("sub").join("inner.txt");
    let dst = data.join("sub").join("renamed.txt");
    let _ = fs::remove_file(&dst);
    let r = fs::rename(&src, &dst);
    let listed = names(&data.join("sub"));
    t.check(
        "rename lower file",
        r.is_ok() && listed == ["renamed.txt"],
        || format!("{r:?} {listed:?}"),
    );
    let _ = fs::rename(&dst, &src);

    // 11. Memory-map a lower file.
    unsafe {
        let p = wide(&data.join("big.bin").to_string_lossy());
        let h = CreateFileW(
            p.as_ptr(),
            0x8000_0000,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        let mut ok = false;
        if h != -1isize as _ {
            let map =
                CreateFileMappingW(h, std::ptr::null(), PAGE_READONLY, 0, 0, std::ptr::null());
            if !map.is_null() {
                let view = MapViewOfFile(map, FILE_MAP_READ, 0, 0, 0);
                if !view.Value.is_null() {
                    let bytes = std::slice::from_raw_parts(view.Value as *const u8, 16 << 20);
                    ok = bytes[4..8] == 1u32.to_le_bytes()
                        && bytes[(12 << 20)..(12 << 20) + 4] == ((12u32 << 20) / 4).to_le_bytes();
                    UnmapViewOfFile(view);
                }
                CloseHandle(map);
            }
            CloseHandle(h);
        }
        t.check("memory-map lower file", ok, || {
            "mapping failed or data wrong".into()
        });
    }

    // 12. Load a DLL from the layer and call it.
    unsafe {
        let p = wide(&app.join("cellanswer.dll").to_string_lossy());
        let h = LoadLibraryW(p.as_ptr());
        let mut got = -1;
        if !h.is_null() {
            if let Some(f) = GetProcAddress(h, b"cell_answer\0".as_ptr()) {
                let f: extern "C" fn() -> i32 = std::mem::transmute(f);
                got = f();
            }
        }
        t.check("load DLL from layer", got == 42, || {
            format!("handle null={} got={got}", h.is_null())
        });
    }

    // 13. The raw data layer at /assets.
    let asset = fs::read_to_string(root.join("assets").join("level1.txt"));
    let listed = names(&root);
    t.check(
        "data layer mounted at /assets",
        asset.as_deref().ok().map(str::trim) == Some("level one")
            && listed.iter().any(|n| n == "assets"),
        || format!("{asset:?} {listed:?}"),
    );

    // 14. A child process sees the same root.
    let out = Command::new("cmd")
        .args(["/c", "type"])
        .arg(data.join("hello.txt"))
        .output();
    let child_ok = out
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("hello from the layer"))
        .unwrap_or(false);
    t.check("child process reads the layer", child_ok, || {
        format!("{out:?}")
    });

    // 15. The private home.
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let saved = fs::write(Path::new(&appdata).join("celltest-save.txt"), "save");
    t.check(
        "private home is writable",
        saved.is_ok() && appdata.starts_with(&home),
        || format!("{saved:?} home={home} appdata={appdata}"),
    );

    // 16. The sandbox. CELL_SANDBOX says what the launcher did.
    let in_container = unsafe {
        let mut tok = std::ptr::null_mut();
        let mut v: u32 = 0;
        let mut n: u32 = 0;
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) != 0
            && GetTokenInformation(tok, TokenIsAppContainer, &mut v as *mut _ as *mut _, 4, &mut n) != 0
            && v != 0
    };
    let want_container = std::env::var("CELL_SANDBOX").as_deref() == Ok("appcontainer");
    println!("CELL_SANDBOX: {}", std::env::var("CELL_SANDBOX").unwrap_or_default());
    t.check(
        "token is an AppContainer when the sandbox is on",
        in_container == want_container,
        || format!("in_container={in_container} want={want_container}"),
    );

    // 17. Network: the manifest says none, so a connect must fail at once
    // inside the container. Outside it the connect reaches the host stack.
    let addr: std::net::SocketAddr = "1.1.1.1:53".parse().unwrap();
    let start = std::time::Instant::now();
    let r = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3));
    let blocked = r.is_err() && start.elapsed() < std::time::Duration::from_millis(500);
    if want_container {
        t.check("network blocked in the container", blocked, || format!("{r:?} after {:?}", start.elapsed()));
    } else {
        println!("INFO network outside the sandbox: {:?} after {:?}", r.is_ok(), start.elapsed());
    }

    // 18. The declared registry keys: inline values and the .reg import,
    // every type, with $CELL_ROOT expanded.
    let key = "Software\\CellTest";
    let imported = "Software\\CellTest\\Imported";
    let want_dir = format!("{}/app", root.display());
    let install = reg_string(key, "InstallDir");
    let default = reg_string(key, "");
    let full = reg_dword(key, "Fullscreen");
    let greeting = reg_string(imported, "Greeting");
    let level = reg_dword(imported, "Level");
    let big = reg_get(imported, "Big");
    let blob = reg_get(imported, "Blob");
    let path = reg_string(imported, "Path");
    let list = reg_get(imported, "List");
    let seeded_ok = install.as_ref().ok() == Some(&(REG_SZ, want_dir.clone()))
        && default.as_ref().ok() == Some(&(REG_SZ, "default".to_string()))
        && full == Ok(0)
        && greeting.as_ref().ok() == Some(&(REG_SZ, "hello \"cell\"".to_string()))
        && level == Ok(7)
        && big.as_ref().ok().map(|(_, b)| b.as_slice()) == Some(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0])
        && blob.as_ref().ok().map(|(_, b)| b.as_slice()) == Some(&[1, 2, 3])
        && path.as_ref().ok() == Some(&(REG_EXPAND_SZ, "%APPDATA%".to_string()))
        && list.as_ref().ok().map(|(k, b)| (*k, b.len())) == Some((7, 12));
    t.check("declared registry values are present", seeded_ok, || {
        format!(
            "InstallDir={install:?} (want {want_dir}) default={default:?} Fullscreen={full:?} \
             Greeting={greeting:?} Level={level:?} Big={big:?} Blob={blob:?} Path={path:?} List={list:?}"
        )
    });

    // 19. A declared key is writable and keeps its values between runs.
    let runs = reg_dword(key, "Runs").unwrap_or(0);
    let wrote = reg_set_dword(key, "Runs", runs + 1);
    let back = reg_dword(key, "Runs");
    println!("registry runs so far: {}", back.clone().unwrap_or(0));
    t.check(
        "declared registry key is writable",
        wrote.is_ok() && back == Ok(runs + 1),
        || format!("write={wrote:?} read={back:?}"),
    );

    // 20. A grant-only key is writable too.
    let shared = reg_set_dword("Software\\CellTestShared", "Seen", 1);
    t.check("granted registry key is writable", shared.is_ok(), || {
        format!("{shared:?}")
    });

    // 21. A key the cell did not declare: blocked in the container, open
    // outside it (and removed again so the test leaves nothing behind).
    let denied = reg_set_dword("Software\\CellTestDenied", "x", 1);
    if want_container {
        t.check(
            "undeclared registry key is blocked in the container",
            denied == Err(5),
            || format!("{denied:?}"),
        );
    } else {
        println!("INFO undeclared registry key outside the sandbox: {denied:?}");
    }
    if denied.is_ok() {
        let k = wide("Software\\CellTestDenied");
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, k.as_ptr()) };
    }

    println!("{} passed, {} failed", t.passed, t.failed);
    std::process::exit(if t.failed == 0 { 0 } else { 1 });
}

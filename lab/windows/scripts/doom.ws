// Log on at the console, start the Windows Doom cell from the Run dialog
// and take screenshots along the way, so a GUI app can be seen running
// from the WinFsp volume. Run from lab/windows:
//
//   vmlab script scripts/doom.ws
//
// The cell must already be at C:\cell\win-doom.cell.exe in the guest.

use vmlab

fn main(lab: Lab) {
    let Ok(m) = lab.vm("win") else { return }
    let s1 = m.screenshot("shots/01-before.png")
    let k1 = m.send_keys("space")
    vmlab::sleep_ms(3000)
    let s2 = m.screenshot("shots/02-signin.png")
    let t1 = m.type_text("vmlab123!\n")
    vmlab::sleep_ms(25000)
    let s3 = m.screenshot("shots/03-desktop.png")
    let k2 = m.send_keys("super-r")
    vmlab::sleep_ms(3000)
    let t2 = m.type_text("C:/cell/win-doom.cell.exe\n")
    vmlab::sleep_ms(20000)
    let s4 = m.screenshot("shots/04-doom.png")
}

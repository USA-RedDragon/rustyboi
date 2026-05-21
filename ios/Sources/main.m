// rustyboi iOS entry shim.
//
// The real entry point lives in the Rust staticlib (rustyboi-platform): calling
// `rustyboi_ios_main` hands control to the shared winit GUI loop, whose UIKit
// backend calls `UIApplicationMain` internally. So `main()` here is just a
// trampoline into Rust — there is no AppDelegate or storyboard; winit creates
// the UIWindow and view controller itself.
extern int rustyboi_ios_main(void);

int main(int argc, char *argv[]) {
    (void)argc;
    (void)argv;
    return rustyboi_ios_main();
}

#import <AppKit/AppKit.h>
#import <dispatch/dispatch.h>
#import <dlfcn.h>
#import <mpv/client.h>

static NSWindow *probe_window;
static unsigned last_id;

static void post_key(void) {
    NSWindow *key_window = NSApp.keyWindow;
    if (NSApp.modalWindow || key_window.sheetParent || [key_window isKindOfClass:NSSavePanel.class]) {
        // The Cmd+O check runs last; acknowledge the real file panel.
        FILE *modal = fopen(getenv("COFFEE_TEST_MODAL"), "w");
        if (modal) fclose(modal);
        return;
    }

    FILE *request = fopen(getenv("COFFEE_TEST_KEY"), "r");
    if (!request) return;
    unsigned id, code, cp, command;
    int fields = fscanf(request, "%u %u %x %u", &id, &code, &cp, &command);
    fclose(request);
    if (fields != 4 || last_id == id) return;

    if (!NSApp.isActive || !probe_window.isKeyWindow) {
        [NSApp activateIgnoringOtherApps:YES];
        [probe_window makeKeyAndOrderFront:nil];
        return;
    }
    last_id = id;
    unichar character = (unichar)cp;
    NSString *text = [NSString stringWithCharacters:&character length:1];
    for (NSEventType type = NSEventTypeKeyDown; type <= NSEventTypeKeyUp; type++) {
        [NSApp postEvent:[NSEvent keyEventWithType:type location:NSZeroPoint
            modifierFlags:command ? NSEventModifierFlagCommand : 0
            timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:probe_window.windowNumber
            context:nil characters:text charactersIgnoringModifiers:text isARepeat:NO
            keyCode:(unsigned short)code] atStart:NO];
    }
}

int mpv_initialize(mpv_handle *mpv) {
    void *provider = dlopen(getenv("COFFEE_TEST_PROVIDER"), RTLD_NOW | RTLD_FIRST);
    int (*initialize)(mpv_handle *) = provider ? dlsym(provider, "mpv_initialize") : NULL;
    if (!initialize) return MPV_ERROR_GENERIC;
    if (mpv_set_option_string(mpv, "input-ipc-server", getenv("COFFEE_TEST_IPC")) < 0)
        return MPV_ERROR_GENERIC;
    mpv_set_option_string(mpv, "ao", "null");
    mpv_set_option_string(mpv, "vo", "null");
    dispatch_async(dispatch_get_main_queue(), ^{
        // Keep native AppKit event delivery, independent of CI GPU support.
        probe_window = [[NSWindow alloc] initWithContentRect:NSMakeRect(150, 150, 480, 270)
            styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable
            backing:NSBackingStoreBuffered defer:NO];
        probe_window.title = @"Coffee keyboard test";
        [probe_window makeKeyAndOrderFront:nil];
        NSTimer *timer = [NSTimer timerWithTimeInterval:.05 repeats:YES block:^(NSTimer *_) {
            post_key();
        }];
        [NSRunLoop.mainRunLoop addTimer:timer forMode:NSRunLoopCommonModes];
        [NSRunLoop.mainRunLoop addTimer:timer forMode:NSModalPanelRunLoopMode];
    });
    return initialize(mpv);
}

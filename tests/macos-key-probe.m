#import <AppKit/AppKit.h>
#import <dispatch/dispatch.h>
#import <dlfcn.h>
#import <mpv/client.h>

static dispatch_source_t timer;
static unsigned long last_request;

static void observe_key(NSEvent *event) {
    FILE *log = fopen(getenv("COFFEE_PROBE_EVENTS"), "a");
    if (log) {
        fprintf(log, "%lu %ld %u %d %d\n", last_request, (long)event.type,
                event.keyCode, NSApp.isActive, event.window.isKeyWindow);
        fclose(log);
    }
}

static void post_keys(void) {
    FILE *request = fopen(getenv("COFFEE_PROBE_REQUEST"), "r");
    if (!request) return;
    unsigned long id;
    unsigned short code;
    unsigned int cp;
    int items = fscanf(request, "%lu %hu %x", &id, &code, &cp);
    fclose(request);
    if (items != 3 || last_request == id || NSApp.modalWindow) return;

    NSWindow *window = NSApp.keyWindow ?: NSApp.windows.firstObject;
    if (!window.isVisible) return;
    if (!NSApp.isActive || !window.isKeyWindow) {
        [NSApp activateIgnoringOtherApps:YES];
        [window makeKeyAndOrderFront:nil];
        return;
    }
    last_request = id;
    unichar value = (unichar)cp;
    NSString *chars = [NSString stringWithCharacters:&value length:1];
    for (NSEventType type = NSEventTypeKeyDown; type <= NSEventTypeKeyUp; type++) {
        NSEvent *event = [NSEvent keyEventWithType:type
            location:NSZeroPoint modifierFlags:0 timestamp:NSProcessInfo.processInfo.systemUptime
            windowNumber:window.windowNumber context:nil characters:chars
            charactersIgnoringModifiers:chars isARepeat:NO keyCode:code];
        [NSApp postEvent:event atStart:NO];
    }
}

int mpv_initialize(mpv_handle *mpv) {
    // Resolve the actual provider's implementation, not this wrapper.
    void *provider = dlopen(getenv("COFFEE_PROBE_PROVIDER"), RTLD_NOW | RTLD_FIRST);
    int (*initialize)(mpv_handle *) = provider ? dlsym(provider, "mpv_initialize") : NULL;
    if (!initialize) return MPV_ERROR_GENERIC;
    if (mpv_set_option_string(mpv, "input-ipc-server", getenv("COFFEE_PROBE_SOCKET")) < 0)
        return MPV_ERROR_GENERIC;
    mpv_set_option_string(mpv, "ao", "null");
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskKeyDown | NSEventMaskKeyUp
            handler:^NSEvent *(NSEvent *event) {
                observe_key(event);
                return event; // Observe without changing delivery.
            }];
        timer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, dispatch_get_main_queue());
        dispatch_source_set_timer(timer, DISPATCH_TIME_NOW, 40 * NSEC_PER_MSEC, 5 * NSEC_PER_MSEC);
        dispatch_source_set_event_handler(timer, ^{ post_keys(); });
        dispatch_resume(timer);
    });
    return initialize(mpv);
}

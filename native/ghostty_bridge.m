// Relay's macOS-only prototype adapter for the pinned Ghostty 1.3.1 C API.
// NSView, Ghostty app/surface calls, and destruction run on the main thread.
// Only wakeup_cb crosses threads; it invokes Rust's thread-safe repaint callback.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <IOSurface/IOSurface.h>
#include <ghostty.h>
#include <math.h>

typedef struct {
    ghostty_app_t app;
    ghostty_config_t config;
    __strong NSView *parent;
    void *userdata;
    void (*wake)(void *);
} RelayGhostty;

@interface RelayGhosttyView : NSView <NSTextInputClient> {
    NSString *_marked;
    NSEvent *_keyEvent;
    NSTrackingArea *_tracking;
}
@property(nonatomic, assign) ghostty_surface_t surface;
@property(nonatomic, assign) RelayGhostty *runtime;
@property(nonatomic, assign) BOOL ended;
@property(nonatomic, assign) int exitCode;
@end

static ghostty_input_mods_e mods(NSEventModifierFlags flags) {
    int value = 0;
    if (flags & NSEventModifierFlagShift) value |= GHOSTTY_MODS_SHIFT;
    if (flags & NSEventModifierFlagControl) value |= GHOSTTY_MODS_CTRL;
    if (flags & NSEventModifierFlagOption) value |= GHOSTTY_MODS_ALT;
    if (flags & NSEventModifierFlagCommand) value |= GHOSTTY_MODS_SUPER;
    if (flags & NSEventModifierFlagCapsLock) value |= GHOSTTY_MODS_CAPS;
    return value;
}

@implementation RelayGhosttyView
- (BOOL)isFlipped { return YES; }
- (BOOL)acceptsFirstResponder { return YES; }
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }
- (BOOL)becomeFirstResponder {
    if (self.surface) ghostty_surface_set_focus(self.surface, true);
    return YES;
}
- (BOOL)resignFirstResponder {
    if (self.surface) ghostty_surface_set_focus(self.surface, false);
    return YES;
}
- (void)updateTrackingAreas {
    [super updateTrackingAreas];
    if (_tracking) [self removeTrackingArea:_tracking];
    _tracking = [[NSTrackingArea alloc] initWithRect:NSZeroRect
        options:NSTrackingMouseMoved | NSTrackingActiveInKeyWindow | NSTrackingInVisibleRect
        owner:self userInfo:nil];
    [self addTrackingArea:_tracking];
}
- (BOOL)sendKey:(NSEvent *)event text:(NSString *)text action:(ghostty_input_action_e)action {
    if (!self.surface) return NO;
    ghostty_input_key_s key = {0};
    key.action = action;
    key.mods = mods(event.modifierFlags);
    key.keycode = event.keyCode;
    key.text = text.length ? text.UTF8String : NULL;
    NSString *plain = event.charactersIgnoringModifiers;
    key.unshifted_codepoint = plain.length ? [plain characterAtIndex:0] : 0;
    key.composing = self.hasMarkedText;
    // Preserve copy/selection bindings after exit without forwarding ordinary
    // typing to Ghostty's "press any key to close" path.
    if (self.ended && !ghostty_surface_key_is_binding(self.surface, key, NULL)) return NO;
    return ghostty_surface_key(self.surface, key);
}
- (BOOL)performKeyEquivalent:(NSEvent *)event {
    if (self.window.firstResponder != self || !(event.modifierFlags & NSEventModifierFlagCommand)) return NO;
    return [self sendKey:event text:nil action:GHOSTTY_ACTION_PRESS];
}
- (void)keyDown:(NSEvent *)event {
    if (event.modifierFlags & (NSEventModifierFlagControl | NSEventModifierFlagCommand)) {
        [self sendKey:event text:nil action:event.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS];
        return;
    }
    _keyEvent = event;
    [self interpretKeyEvents:@[event]];
    _keyEvent = nil;
}
- (void)keyUp:(NSEvent *)event { [self sendKey:event text:nil action:GHOSTTY_ACTION_RELEASE]; }
- (void)doCommandBySelector:(SEL)selector {
    if (_keyEvent) [self sendKey:_keyEvent text:nil action:_keyEvent.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS];
}
- (void)insertText:(id)value replacementRange:(NSRange)range {
    NSString *text = [value isKindOfClass:NSAttributedString.class] ? [value string] : value;
    [self unmarkText];
    if (_keyEvent) [self sendKey:_keyEvent text:text action:_keyEvent.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS];
    else if (self.surface) ghostty_surface_text(self.surface, text.UTF8String, [text lengthOfBytesUsingEncoding:NSUTF8StringEncoding]);
}
- (void)setMarkedText:(id)value selectedRange:(NSRange)selected replacementRange:(NSRange)replacement {
    _marked = [value isKindOfClass:NSAttributedString.class] ? [value string] : value;
    if (self.surface) ghostty_surface_preedit(self.surface, _marked.UTF8String, [_marked lengthOfBytesUsingEncoding:NSUTF8StringEncoding]);
}
- (void)unmarkText {
    _marked = nil;
    if (self.surface) ghostty_surface_preedit(self.surface, NULL, 0);
}
- (BOOL)hasMarkedText { return _marked.length > 0; }
- (NSRange)markedRange { return self.hasMarkedText ? NSMakeRange(0, _marked.length) : NSMakeRange(NSNotFound, 0); }
- (NSRange)selectedRange { return NSMakeRange(NSNotFound, 0); }
- (NSArray<NSAttributedStringKey> *)validAttributesForMarkedText { return @[]; }
- (NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range actualRange:(NSRangePointer)actual {
    if (actual) *actual = NSMakeRange(NSNotFound, 0);
    return nil;
}
- (NSUInteger)characterIndexForPoint:(NSPoint)point { return NSNotFound; }
- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actual {
    double x = 0, y = 0, w = 1, h = 14;
    if (self.surface) ghostty_surface_ime_point(self.surface, &x, &y, &w, &h);
    if (actual) *actual = self.markedRange;
    return [self.window convertRectToScreen:[self convertRect:NSMakeRect(x, y, w, h) toView:nil]];
}
- (void)mouseMoved:(NSEvent *)event {
    NSPoint p = [self convertPoint:event.locationInWindow fromView:nil];
    if (self.surface) ghostty_surface_mouse_pos(self.surface, p.x, p.y, mods(event.modifierFlags));
}
- (void)mouseButton:(NSEvent *)event down:(BOOL)down button:(ghostty_input_mouse_button_e)button {
    if (!self.surface) return;
    if (down) [self.window makeFirstResponder:self];
    [self mouseMoved:event];
    ghostty_surface_mouse_button(self.surface, down ? GHOSTTY_MOUSE_PRESS : GHOSTTY_MOUSE_RELEASE,
                                button, mods(event.modifierFlags));
}
- (void)mouseDown:(NSEvent *)event { [self mouseButton:event down:YES button:GHOSTTY_MOUSE_LEFT]; }
- (void)mouseUp:(NSEvent *)event { [self mouseButton:event down:NO button:GHOSTTY_MOUSE_LEFT]; }
- (void)rightMouseDown:(NSEvent *)event { [self mouseButton:event down:YES button:GHOSTTY_MOUSE_RIGHT]; }
- (void)rightMouseUp:(NSEvent *)event { [self mouseButton:event down:NO button:GHOSTTY_MOUSE_RIGHT]; }
- (void)otherMouseDown:(NSEvent *)event { [self mouseButton:event down:YES button:GHOSTTY_MOUSE_MIDDLE]; }
- (void)otherMouseUp:(NSEvent *)event { [self mouseButton:event down:NO button:GHOSTTY_MOUSE_MIDDLE]; }
- (void)mouseDragged:(NSEvent *)event { [self mouseMoved:event]; }
- (void)rightMouseDragged:(NSEvent *)event { [self mouseMoved:event]; }
- (void)otherMouseDragged:(NSEvent *)event { [self mouseMoved:event]; }
- (void)scrollWheel:(NSEvent *)event {
    if (!self.surface) return;
    [self mouseMoved:event];
    int momentum = GHOSTTY_MOUSE_MOMENTUM_NONE;
    if (event.momentumPhase & NSEventPhaseBegan) momentum = GHOSTTY_MOUSE_MOMENTUM_BEGAN;
    else if (event.momentumPhase & NSEventPhaseChanged) momentum = GHOSTTY_MOUSE_MOMENTUM_CHANGED;
    else if (event.momentumPhase & NSEventPhaseEnded) momentum = GHOSTTY_MOUSE_MOMENTUM_ENDED;
    else if (event.momentumPhase & NSEventPhaseCancelled) momentum = GHOSTTY_MOUSE_MOMENTUM_CANCELLED;
    BOOL precise = event.hasPreciseScrollingDeltas;
    ghostty_surface_mouse_scroll(self.surface, event.scrollingDeltaX * (precise ? 2 : 1),
        event.scrollingDeltaY * (precise ? 2 : 1), (precise ? 1 : 0) | (momentum << 1));
}
@end

static void wakeup(void *userdata) {
    RelayGhostty *runtime = userdata;
    runtime->wake(runtime->userdata);
}
static bool action(ghostty_app_t app, ghostty_target_s target, ghostty_action_s action) {
    (void)app;
    if (target.tag != GHOSTTY_TARGET_SURFACE) return false;
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ghostty_surface_userdata(target.target.surface);
    switch (action.tag) {
        case GHOSTTY_ACTION_SHOW_CHILD_EXITED:
            view.ended = YES;
            view.exitCode = (int)action.action.child_exited.exit_code;
            wakeup(view.runtime);
            return true;
        case GHOSTTY_ACTION_RENDER:
            ghostty_surface_draw(target.target.surface);
            return true;
        case GHOSTTY_ACTION_SET_TITLE:
        case GHOSTTY_ACTION_SET_TAB_TITLE:
            return true; // Relay uses the saved connection name for tab titles.
        case GHOSTTY_ACTION_RING_BELL:
            NSBeep();
            return true;
        default: return false;
    }
}
static bool read_clipboard(void *userdata, ghostty_clipboard_e clipboard, void *state) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)userdata;
    if (!view.surface || clipboard != GHOSTTY_CLIPBOARD_STANDARD) return false;
    NSString *text = [NSPasteboard.generalPasteboard stringForType:NSPasteboardTypeString] ?: @"";
    ghostty_surface_complete_clipboard_request(view.surface, text.UTF8String, state, false);
    return true;
}
static void confirm_read(void *userdata, const char *text, void *state, ghostty_clipboard_request_e request) {
    (void)text;
    (void)request;
    // Remote requests that require consent receive no local clipboard content.
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)userdata;
    ghostty_surface_complete_clipboard_request(view.surface, "", state, true);
}
static void write_clipboard(void *userdata, ghostty_clipboard_e clipboard,
                            const ghostty_clipboard_content_s *content, size_t count, bool confirm) {
    (void)userdata;
    if (confirm || clipboard != GHOSTTY_CLIPBOARD_STANDARD) return;
    for (size_t i = 0; i < count; i++) {
        if (strcmp(content[i].mime, "text/plain") != 0) continue;
        NSString *text = [NSString stringWithUTF8String:content[i].data];
        if (text) {
            [NSPasteboard.generalPasteboard clearContents];
            [NSPasteboard.generalPasteboard setString:text forType:NSPasteboardTypeString];
        }
        break;
    }
}
static void close_surface(void *userdata, bool alive) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)userdata;
    // A child exit retains its surface/output. Only Relay's tab close frees it.
    if (!alive) { view.ended = YES; wakeup(view.runtime); }
}

// Rust calls these entry points directly. Drain temporary Cocoa/Metal objects
// at the operation boundary instead of retaining them until a later UI event.
// Objects escaping a pool are owned by the runtime or explicitly bridge-retained.
void *relay_ghostty_new(void *parent, const char *config_path, void *userdata, void (*wake)(void *)) {
    @autoreleasepool {
        NSCAssert(NSThread.isMainThread, @"Ghostty must be created on the UI thread");
        static BOOL initialized = NO;
        if (!initialized) {
            char *argv[] = {"relay-ghostty", NULL};
            if (ghostty_init(1, argv) != GHOSTTY_SUCCESS) return NULL;
            initialized = YES;
        }
        RelayGhostty *runtime = calloc(1, sizeof(RelayGhostty));
        if (!runtime) return NULL;
        runtime->parent = (__bridge NSView *)parent;
        runtime->userdata = userdata;
        runtime->wake = wake;
        runtime->config = ghostty_config_new();
        ghostty_config_load_file(runtime->config, config_path);
        ghostty_config_finalize(runtime->config);
        if (ghostty_config_diagnostics_count(runtime->config)) {
            ghostty_config_free(runtime->config);
            runtime->parent = nil;
            free(runtime);
            return NULL;
        }
        ghostty_runtime_config_s callbacks = {0};
        callbacks.userdata = runtime;
        callbacks.wakeup_cb = wakeup;
        callbacks.action_cb = action;
        callbacks.read_clipboard_cb = read_clipboard;
        callbacks.confirm_read_clipboard_cb = confirm_read;
        callbacks.write_clipboard_cb = write_clipboard;
        callbacks.close_surface_cb = close_surface;
        runtime->app = ghostty_app_new(&callbacks, runtime->config);
        if (!runtime->app) {
            ghostty_config_free(runtime->config);
            runtime->parent = nil;
            free(runtime);
            return NULL;
        }
        ghostty_app_set_color_scheme(runtime->app, GHOSTTY_COLOR_SCHEME_LIGHT);
        return runtime;
    }
}
void relay_ghostty_tick(void *ptr) {
    @autoreleasepool {
        RelayGhostty *runtime = ptr;
        ghostty_app_set_focus(runtime->app, runtime->parent.window.isKeyWindow);
        ghostty_app_tick(runtime->app);
    }
}
void relay_ghostty_free(void *ptr) {
    @autoreleasepool {
        RelayGhostty *runtime = ptr;
        ghostty_app_free(runtime->app);
        ghostty_config_free(runtime->config);
        runtime->parent = nil;
        free(runtime);
    }
}
void *relay_ghostty_surface_new(void *ptr, const char *command, const char *directory) {
    @autoreleasepool {
        RelayGhostty *runtime = ptr;
        RelayGhosttyView *view = [[RelayGhosttyView alloc] initWithFrame:NSMakeRect(0, 0, 640, 400)];
        view.runtime = runtime;
        view.exitCode = -1;
        view.hidden = YES;
        [runtime->parent addSubview:view];
        ghostty_surface_config_s config = ghostty_surface_config_new();
        config.platform_tag = GHOSTTY_PLATFORM_MACOS;
        config.platform.macos.nsview = (__bridge void *)view;
        config.userdata = (__bridge void *)view;
        config.scale_factor = runtime->parent.window.backingScaleFactor;
        config.font_size = 14;
        config.command = command;
        config.working_directory = directory;
        config.wait_after_command = true;
        config.context = GHOSTTY_SURFACE_CONTEXT_TAB;
        view.surface = ghostty_surface_new(runtime->app, &config);
        if (!view.surface) { [view removeFromSuperview]; return NULL; }
        // Ghostty surfaces start visible internally, independently of NSView.hidden.
        // hide() short-circuits for a view already hidden, so initialize both states.
        ghostty_surface_set_occlusion(view.surface, false);
        ghostty_surface_set_color_scheme(view.surface, GHOSTTY_COLOR_SCHEME_LIGHT);
        return (__bridge_retained void *)view;
    }
}
void relay_ghostty_surface_free(void *ptr) {
    @autoreleasepool {
        RelayGhosttyView *view = (__bridge_transfer RelayGhosttyView *)ptr;
        if (view.window.firstResponder == view) [view.window makeFirstResponder:view.runtime->parent];
        ghostty_surface_free(view.surface);
        view.surface = NULL;
        [view removeFromSuperview];
    }
}
void relay_ghostty_surface_place(void *ptr, double x, double y, double width, double height, bool focus, bool release_focus) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    NSView *parent = view.runtime->parent;
    if (!parent.isFlipped) y = parent.bounds.size.height - y - height;
    NSRect rect = NSMakeRect(x, y, fmax(1, width), fmax(1, height));
    BOOL changed = !NSEqualRects(view.frame, rect);
    BOOL wasHidden = view.hidden;
    view.frame = rect;
    view.hidden = NO;
    double scale = view.window.backingScaleFactor;
    ghostty_surface_set_content_scale(view.surface, scale, scale);
    if (changed || wasHidden) {
        ghostty_surface_set_size(view.surface, (uint32_t)round(width * scale), (uint32_t)round(height * scale));
        ghostty_surface_set_occlusion(view.surface, true);
        ghostty_surface_refresh(view.surface);
    }
    if (focus || wasHidden) [view.window makeFirstResponder:view];
    if (release_focus && view.window.firstResponder == view) [view.window makeFirstResponder:parent];
}
void relay_ghostty_surface_hide(void *ptr) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    if (view.hidden) return;
    if (view.window.firstResponder == view) [view.window makeFirstResponder:view.runtime->parent];
    view.hidden = YES;
    ghostty_surface_set_focus(view.surface, false);
    ghostty_surface_set_occlusion(view.surface, false);
}
bool relay_ghostty_surface_exited(void *ptr, int *status) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    *status = view.exitCode;
    return view.ended || ghostty_surface_process_exited(view.surface);
}

// Native smoke-test helpers. They exercise the same view methods and callbacks
// used by interactive sessions; Rust exposes them only with terminal-fixture.
size_t relay_ghostty_probe_text(void *ptr, char *output, size_t capacity) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    ghostty_selection_s selection = {0};
    selection.top_left.tag = GHOSTTY_POINT_SCREEN;
    selection.top_left.coord = GHOSTTY_POINT_COORD_TOP_LEFT;
    selection.bottom_right.tag = GHOSTTY_POINT_SCREEN;
    selection.bottom_right.coord = GHOSTTY_POINT_COORD_BOTTOM_RIGHT;
    ghostty_text_s text = {0};
    if (!ghostty_surface_read_text(view.surface, selection, &text)) return 0;
    size_t count = MIN(capacity, text.text_len);
    memcpy(output, text.text, count);
    ghostty_surface_free_text(view.surface, &text);
    return count;
}
void relay_ghostty_probe_key(void *ptr, const char *text, unsigned short keycode, bool control) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    NSString *characters = [NSString stringWithUTF8String:text];
    NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint
        modifierFlags:control ? NSEventModifierFlagControl : 0 timestamp:0
        windowNumber:view.window.windowNumber context:nil characters:characters
        charactersIgnoringModifiers:characters isARepeat:NO keyCode:keycode];
    [view keyDown:event];
}
bool relay_ghostty_probe_clipboard(void *ptr, const char *expected, const char *paste) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    NSPasteboard *pasteboard = NSPasteboard.generalPasteboard;
    NSMutableArray<NSPasteboardItem *> *saved = [NSMutableArray array];
    for (NSPasteboardItem *item in pasteboard.pasteboardItems) {
        NSPasteboardItem *copy = [NSPasteboardItem new];
        for (NSPasteboardType type in item.types) {
            NSData *data = [item dataForType:type];
            if (data) [copy setData:data forType:type];
        }
        [saved addObject:copy];
    }
    ghostty_surface_binding_action(view.surface, "select_all", strlen("select_all"));
    NSEvent *(^shortcut)(NSString *, unsigned short) = ^(NSString *key, unsigned short code) {
        return [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint
            modifierFlags:NSEventModifierFlagCommand timestamp:0 windowNumber:view.window.windowNumber
            context:nil characters:key charactersIgnoringModifiers:key isARepeat:NO keyCode:code];
    };
    BOOL copied = [view performKeyEquivalent:shortcut(@"c", 8)];
    NSString *text = [pasteboard stringForType:NSPasteboardTypeString];
    BOOL matches = copied && [text containsString:[NSString stringWithUTF8String:expected]];
    if (paste) {
        [pasteboard clearContents];
        [pasteboard setString:[NSString stringWithUTF8String:paste] forType:NSPasteboardTypeString];
        matches = [view performKeyEquivalent:shortcut(@"v", 9)] && matches;
    }
    [pasteboard clearContents];
    if (saved.count) [pasteboard writeObjects:saved];
    return matches;
}
bool relay_ghostty_probe_visible(void *ptr) {
    return !((__bridge RelayGhosttyView *)ptr).hidden;
}
bool relay_ghostty_probe_focused(void *ptr) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    return view.window.firstResponder == view;
}
unsigned short relay_ghostty_probe_columns(void *ptr) {
    return ghostty_surface_size(((__bridge RelayGhosttyView *)ptr).surface).columns;
}
size_t relay_ghostty_probe_ink_pixels(void *ptr) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    // Read the completed frame actually presented by Ghostty, rather than its
    // terminal model. This detects blank output after GPU resources are rebuilt.
    for (CALayer *layer in view.layer ? @[view.layer] : @[]) {
        if (![NSStringFromClass(layer.class) isEqualToString:@"IOSurfaceLayer"] || !layer.contents) continue;
        IOSurfaceRef surface = (__bridge IOSurfaceRef)layer.contents;
        if (IOSurfaceGetBytesPerElement(surface) != 4) continue;
        if (IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) continue;
        size_t ink = 0;
        const uint8_t *base = IOSurfaceGetBaseAddress(surface);
        if (base) {
            for (size_t y = 0; y < IOSurfaceGetHeight(surface); y++) {
                const uint8_t *row = base + y * IOSurfaceGetBytesPerRow(surface);
                for (size_t x = 0; x < IOSurfaceGetWidth(surface); x++) {
                    const uint8_t *pixel = row + x * 4;
                    if (pixel[0] + pixel[1] + pixel[2] < 384) ink++;
                }
            }
        }
        IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);
        return ink;
    }
    return 0;
}
void relay_ghostty_probe_scroll(void *ptr, double fraction_x, double lines) {
    RelayGhosttyView *view = (__bridge RelayGhosttyView *)ptr;
    ghostty_surface_mouse_pos(view.surface, view.bounds.size.width * fraction_x,
                             view.bounds.size.height / 2, GHOSTTY_MODS_NONE);
    ghostty_surface_mouse_scroll(view.surface, 0, lines, 0);
}

#import <AppKit/AppKit.h>

@interface BridgeMenu : NSObject <NSApplicationDelegate>
@property (strong) NSStatusItem *statusItem;
@property (assign) void (*requestQuit)(void);
@end

@implementation BridgeMenu
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    self.statusItem = [[NSStatusBar systemStatusBar] statusItemWithLength:NSVariableStatusItemLength];
    NSStatusBarButton *button = self.statusItem.button;
    button.image = [NSImage imageWithSystemSymbolName:@"icloud" accessibilityDescription:@"Zotero Cloud Bridge"];
    if (!button.image) {
        button.title = @"Bridge";
    }
    button.toolTip = @"Zotero Cloud Bridge";

    NSMenu *menu = [[NSMenu alloc] init];
    NSMenuItem *quit = [[NSMenuItem alloc] initWithTitle:@"Quit Zotero Cloud Bridge"
                                               action:@selector(quit:)
                                        keyEquivalent:@""];
    quit.target = self;
    [menu addItem:quit];
    self.statusItem.menu = menu;
}

- (void)quit:(id)sender {
    self.requestQuit();
}
@end

void bridge_run_menu_bar(void (*requestQuit)(void)) {
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
        __attribute__((objc_precise_lifetime)) BridgeMenu *delegate = [[BridgeMenu alloc] init];
        delegate.requestQuit = requestQuit;
        app.delegate = delegate;
        [app run];
        app.delegate = nil;
    }
}

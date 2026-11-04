#import <AppKit/AppKit.h>

@interface Fixture : NSObject <NSApplicationDelegate, NSTextFieldDelegate>
@property NSWindow *window;
@property NSTextField *composer;
@property NSTextField *transcript;
@property NSButton *toggle;
@property NSWindow *duplicate;
@property NSTextField *duplicateState;
@property NSMutableArray<NSString *> *messages;
@property NSMutableArray<NSTextField *> *bubbles;
@end

@implementation Fixture
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    self.messages = [NSMutableArray array];
    self.bubbles = [NSMutableArray array];
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 540, 360)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskResizable
        backing:NSBackingStoreBuffered defer:NO];
    self.window.title = @"ARK Computer fixture";
    self.window.releasedWhenClosed = NO;
    self.window.animationBehavior = NSWindowAnimationBehaviorNone;
    [self.window setAccessibilityIdentifier:@"fixture-window"];
    NSView *content = self.window.contentView;
    NSTextField *recipient = [NSTextField labelWithString:@"\u200eFixture Recipient\u200f"];
    recipient.frame = NSMakeRect(24, 302, 480, 24);
    [recipient setAccessibilityIdentifier:@"fixture-recipient"];
    [content addSubview:recipient];
    self.composer = [[NSTextField alloc] initWithFrame:NSMakeRect(24, 238, 480, 34)];
    self.composer.stringValue = @"old text";
    self.composer.delegate = self;
    self.composer.target = self;
    self.composer.action = @selector(sendMessage:);
    [self.composer setAccessibilityIdentifier:@"fixture-composer"];
    [self.composer setAccessibilityLabel:@"Message"];
    [content addSubview:self.composer];
    self.toggle = [[NSButton alloc] initWithFrame:NSMakeRect(24, 180, 200, 30)];
    [self.toggle setButtonType:NSButtonTypeSwitch];
    self.toggle.title = @"Fixture toggle";
    [self.toggle setAccessibilityIdentifier:@"fixture-toggle"];
    [content addSubview:self.toggle];
    self.transcript = [NSTextField wrappingLabelWithString:@"No outgoing messages"];
    self.transcript.frame = NSMakeRect(24, 24, 480, 130);
    [self.transcript setAccessibilityIdentifier:@"fixture-outgoing"];
    [content addSubview:self.transcript];
    self.duplicateState = [NSTextField labelWithString:@"No duplicate window"];
    self.duplicateState.frame = NSMakeRect(24, 155, 480, 20);
    [self.duplicateState setAccessibilityIdentifier:@"fixture-duplicate-state"];
    [content addSubview:self.duplicateState];
    NSMenu *bar = [[NSMenu alloc] initWithTitle:@"Main"];
    NSMenuItem *application = [[NSMenuItem alloc] initWithTitle:@"Fixture" action:nil keyEquivalent:@""];
    NSMenu *menu = [[NSMenu alloc] initWithTitle:@"Fixture"];
    [menu addItemWithTitle:@"Quit fixture" action:@selector(terminate:) keyEquivalent:@"q"];
    application.submenu = menu;
    [bar addItem:application];
    NSMenuItem *file = [[NSMenuItem alloc] initWithTitle:@"File" action:nil keyEquivalent:@""];
    NSMenu *fileMenu = [[NSMenu alloc] initWithTitle:@"File"];
    NSMenuItem *clear = [[NSMenuItem alloc] initWithTitle:@"Clear transcript" action:@selector(clearTranscript:) keyEquivalent:@""];
    clear.target = self;
    [fileMenu addItem:clear];
    NSMenuItem *replace = [[NSMenuItem alloc] initWithTitle:@"Replace composer" action:@selector(replaceComposer:) keyEquivalent:@""];
    replace.target = self;
    [fileMenu addItem:replace];
    NSMenuItem *recreate = [[NSMenuItem alloc] initWithTitle:@"Recreate window" action:@selector(recreateWindow:) keyEquivalent:@""];
    recreate.target = self;
    [fileMenu addItem:recreate];
    for (NSArray *entry in @[@[@"Grow transcript", @"growTranscript:"], @[@"Hide duplicate", @"hideDuplicate:"], @[@"Close duplicate", @"closeDuplicate:"]]) {
        NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:entry[0] action:NSSelectorFromString(entry[1]) keyEquivalent:@""];
        item.target = self;
        [fileMenu addItem:item];
    }
    file.submenu = fileMenu;
    [bar addItem:file];
    NSMenuItem *edit = [[NSMenuItem alloc] initWithTitle:@"Edit" action:nil keyEquivalent:@""];
    NSMenu *editMenu = [[NSMenu alloc] initWithTitle:@"Edit"];
    [editMenu addItemWithTitle:@"Select All" action:@selector(selectAll:) keyEquivalent:@"a"];
    edit.submenu = editMenu;
    [bar addItem:edit];
    NSApp.mainMenu = bar;
    [self.window center];
    [self.window makeKeyAndOrderFront:nil];
    [self.window makeFirstResponder:self.composer];
    [NSApp activateIgnoringOtherApps:YES];
}
- (BOOL)control:(NSControl *)control textView:(NSTextView *)textView doCommandBySelector:(SEL)command {
    if (command == @selector(insertNewline:)) {
        [self sendMessage:control];
        return YES;
    }
    return NO;
}
- (void)sendMessage:(id)sender {
    NSString *text = self.composer.stringValue;
    if (text.length == 0) { return; }
    NSString *message = [NSString stringWithFormat:@"Your message %@ to Fixture Recipient", text];
    [self.messages addObject:message];
    self.transcript.stringValue = [NSString stringWithFormat:@"%lu outgoing messages", (unsigned long)self.messages.count];
    NSTextField *bubble = [NSTextField labelWithString:message];
    bubble.frame = NSMakeRect(24, 130 - 24 * self.messages.count, 480, 24);
    [bubble setAccessibilityIdentifier:[NSString stringWithFormat:@"fixture-message-%lu", (unsigned long)self.messages.count]];
    [self.window.contentView addSubview:bubble];
    [self.bubbles addObject:bubble];
    self.composer.stringValue = @"";
    [self.window makeFirstResponder:self.composer];
}
- (void)clearTranscript:(id)sender {
    [self.messages removeAllObjects];
    for (NSTextField *bubble in self.bubbles) { [bubble removeFromSuperview]; }
    [self.bubbles removeAllObjects];
    self.transcript.stringValue = @"No outgoing messages";
}
- (void)replaceComposer:(id)sender {
    NSRect frame = self.composer.frame;
    [self.composer removeFromSuperview];
    self.composer = [[NSTextField alloc] initWithFrame:frame];
    self.composer.stringValue = @"replacement object";
    self.composer.delegate = self;
    self.composer.target = self;
    self.composer.action = @selector(sendMessage:);
    [self.composer setAccessibilityIdentifier:@"fixture-composer"];
    [self.composer setAccessibilityLabel:@"Message"];
    [self.window.contentView addSubview:self.composer];
    [self.window makeFirstResponder:self.composer];
}
- (void)recreateWindow:(id)sender {
    NSWindow *old = self.window;
    NSView *content = old.contentView;
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 540, 360)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskResizable
        backing:NSBackingStoreBuffered defer:NO];
    self.window.title = @"ARK Computer fixture";
    self.window.releasedWhenClosed = NO;
    self.window.animationBehavior = NSWindowAnimationBehaviorNone;
    [self.window setAccessibilityIdentifier:@"fixture-window"];
    old.contentView = nil;
    self.window.contentView = content;
    [self.window center];
    [self.window makeKeyAndOrderFront:nil];
    [self.window makeFirstResponder:self.composer];
    [old close];
}
- (void)growTranscript:(id)sender {
    for (NSUInteger i = 0; i < 220; ++i) {
        NSTextField *line = [NSTextField labelWithString:[NSString stringWithFormat:@"Historical entry %lu with enough text to exceed the public observation budget", (unsigned long)i]];
        line.frame = NSMakeRect(24, -500 - 20 * i, 480, 20);
        [line setAccessibilityIdentifier:[NSString stringWithFormat:@"fixture-history-%lu", (unsigned long)i]];
        [self.window.contentView addSubview:line];
    }
}
- (void)hideDuplicate:(id)sender {
    self.duplicate = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 400, 200) styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    self.duplicate.releasedWhenClosed = NO;
    self.duplicate.animationBehavior = NSWindowAnimationBehaviorNone;
    self.duplicate.title = @"Hidden fixture conversation";
    NSTextField *recipient = [NSTextField labelWithString:@"Fixture Recipient"];
    recipient.frame = NSMakeRect(10, 120, 350, 24);
    [recipient setAccessibilityIdentifier:@"fixture-recipient"];
    [self.duplicate.contentView addSubview:recipient];
    NSTextField *composer = [[NSTextField alloc] initWithFrame:NSMakeRect(10, 60, 350, 30)];
    composer.stringValue = @"Recover me";
    [composer setAccessibilityIdentifier:@"fixture-composer"];
    [composer setAccessibilityLabel:@"Message"];
    [self.duplicate.contentView addSubview:composer];
    [self.duplicate orderOut:nil];
    self.duplicateState.stringValue = @"Hidden duplicate window";
}
- (void)closeDuplicate:(id)sender {
    [self.duplicate close];
    self.duplicate = nil;
    self.duplicateState.stringValue = @"No duplicate window";
}
- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)sender { return YES; }
@end

int main(void) {
    @autoreleasepool {
        NSApplication *application = [NSApplication sharedApplication];
        [application setActivationPolicy:NSApplicationActivationPolicyRegular];
        Fixture *fixture = [Fixture new];
        application.delegate = fixture;
        [application run];
    }
    return 0;
}

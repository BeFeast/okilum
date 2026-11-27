#import <AppKit/AppKit.h>

// Lookup only: never registers, launches or changes the user's protocol handler.
bool okilum_t3_protocol_registered(void) {
    NSURL *url = [NSURL URLWithString:@"t3code://threads/environment/thread"];
    return [[NSWorkspace sharedWorkspace] URLForApplicationToOpenURL:url] != nil;
}

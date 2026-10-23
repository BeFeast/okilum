// Hosted-only integration host. The production driver and real Sparkle own
// version selection, network, archive validation, extraction and installation.
// These overrides automate the two standard UI choices; they do not select a
// version or fabricate a Sparkle state/reply.
#import "../../../crates/okilum-shell/src/updater/bridge.m"

static void Trace(NSString *event) {
    NSString *path = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSTrace"];
    NSFileHandle *file = [NSFileHandle fileHandleForWritingAtPath:path];
    [file seekToEndOfFile];
    [file writeData:[[event stringByAppendingString:@"\n"] dataUsingEncoding:NSUTF8StringEncoding]];
    [file closeFile];
}
static void Report(int kind, const char *text) {
    Trace([NSString stringWithFormat:@"report %d %s", kind, text ?: ""]);
}
@interface TestDriver : TSUserDriver
@end
@implementation TestDriver
- (void)showUpdaterError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    Trace([@"Sparkle error: " stringByAppendingString:error.localizedDescription]);
    acknowledgement();
}
- (void)showUpdateNotFoundWithError:(NSError *)error acknowledgement:(void (^)(void))acknowledgement {
    acknowledgement();
}
- (void)dismissUpdateInstallation {
    Trace([@"dismiss " stringByAppendingString:self.item.versionString ?: @"none"]);
    [super dismissUpdateInstallation];
}
- (void)presentUpdate:(SUAppcastItem *)item state:(SPUUserUpdateState *)state reply:(void (^)(SPUUserUpdateChoice))reply {
    Trace([@"offer " stringByAppendingString:item.versionString]);
    if (self.replacing) [super presentUpdate:item state:state reply:reply];
    else reply(SPUUserUpdateChoiceInstall); // automated click on Install
}
- (void)showDownloadDidStartExtractingUpdate {
    Trace([@"downloaded " stringByAppendingString:self.item.versionString]);
    [super showDownloadDidStartExtractingUpdate];
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSQuitDuringDownload"] boolValue]) {
        Trace(@"quit during download/extraction");
        [NSApp terminate:nil];
    }
}
- (void)showReadyToInstallAndRelaunch:(void (^)(SPUUserUpdateChoice))reply {
    Trace([@"prepared " stringByAppendingString:self.item.versionString]);
    if ([self.item.versionString isEqualToString:@"2"]) {
        // Publish only AFTER the real N download/extraction. The unchanged-feed
        // control copies the same feed; all scenarios use this exact hook.
        NSString *pending = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSPendingFeed"];
        NSData *data = [NSData dataWithContentsOfFile:pending];
        NSString *feed = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSFeedPath"];
        if (![data writeToFile:feed atomically:YES]) exit(31);
        Trace(@"published feed");
    }
    [super showReadyToInstallAndRelaunch:reply];
}
- (void)presentReady:(void (^)(SPUUserUpdateChoice))reply {
    Trace([@"verified-ready " stringByAppendingString:self.item.versionString]);
    NSString *restartFeed = [NSBundle.mainBundle objectForInfoDictionaryKey:@"TSRestartFeed"];
    if (restartFeed && [self.item.versionString isEqualToString:@"2"]) {
        NSData *data = [NSData dataWithContentsOfFile:restartFeed];
        if (![data writeToFile:[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSFeedPath"] atomically:YES]) exit(34);
        Trace(@"published while ready");
    }
    if ([[NSBundle.mainBundle objectForInfoDictionaryKey:@"TSQuit"] boolValue]) {
        [NSApp terminate:nil]; return;
    }
    reply(SPUUserUpdateChoiceInstall); // automated click on Restart
}
@end
@interface TestApplication : NSObject <NSApplicationDelegate>
@end
@implementation TestApplication
- (void)applicationWillTerminate:(NSNotification *)notification {
    if (NSApp.delegate != self) exit(35);
    Trace(@"original delegate restored for shutdown");
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    Trace(@"termination requested");
    return NSTerminateNow;
}
@end
int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSBundle *bundle = NSBundle.mainBundle;
        NSString *version = [bundle objectForInfoDictionaryKey:@"CFBundleVersion"];
        if (![version isEqualToString:@"1"]) {
            Trace([@"relaunched " stringByAppendingString:version]);
            NSString *receipt = [bundle objectForInfoDictionaryKey:@"TSReceipt"];
            [version writeToFile:receipt atomically:YES encoding:NSUTF8StringEncoding error:NULL];
            return 0;
        }
        TestApplication *delegate = [TestApplication new];
        NSApp.delegate = delegate;
        TSRegisterChannelDefault();
        // Also exercise migration: an old persisted automatic-download choice
        // must not bypass the production hook once the new plist disallows it.
        [NSUserDefaults.standardUserDefaults setBool:YES forKey:@"SUAutomaticallyUpdate"];
        [NSUserDefaults.standardUserDefaults setBool:[[bundle objectForInfoDictionaryKey:@"TSBeta"] boolValue] forKey:TSBetaKey];
        TSReporter = Report;
        TSStart(bundle, [[TestDriver alloc] initWithHostBundle:bundle delegate:nil]);
        if (TSUpdater.automaticallyDownloadsUpdates || !TSUpdater.automaticallyChecksForUpdates) return 32;
        // Start in the same runloop ordering as production's manual Install.
        [TSUpdater checkForUpdates];
        [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
            if (okilum_updater_guard_requested()) {
                Trace(@"final editor guard boundary");
                if ([[bundle objectForInfoDictionaryKey:@"TSRefuse"] boolValue]) okilum_updater_refuse_restart();
                else if (!okilum_updater_finish_restart()) exit(33);
            }
        }];
        [NSApp run];
    }
    return 0;
}

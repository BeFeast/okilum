// Isolated AppKit pasteboard tests: no Services registration or app activation.
#import "bridge.m"
#include <assert.h>
static NSString *received;
static void record(const char *url) { received = [NSString stringWithUTF8String:url]; }
int main(void) {
    @autoreleasepool {
        NSString *root = [NSTemporaryDirectory() stringByAppendingPathComponent:NSUUID.UUID.UUIDString];
        NSString *folder = [root stringByAppendingPathComponent:@"Vault space Заметки % #"];
        NSFileManager *files = NSFileManager.defaultManager;
        assert([files createDirectoryAtPath:folder withIntermediateDirectories:YES attributes:nil error:NULL]);
        NSString *note = [root stringByAppendingPathComponent:@"note.md"];
        assert([@"original" writeToFile:note atomically:YES encoding:NSUTF8StringEncoding error:NULL]);
        NSPasteboard *board = [NSPasteboard pasteboardWithUniqueName];
        OkilumFolderService *provider = [OkilumFolderService new];
        folderOpen = record;
        for (NSNumber *legacy in @[@NO, @YES]) {
            [board clearContents];
            if (legacy.boolValue) {
                [board declareTypes:@[@"NSFilenamesPboardType"] owner:nil];
                assert([board setPropertyList:@[folder] forType:@"NSFilenamesPboardType"]);
            } else {
                assert([board writeObjects:@[[NSURL fileURLWithPath:folder]]]);
            }
            NSString *error = nil;
            received = nil;
            [provider openFolderAsVault:board userData:nil error:&error];
            assert(!error && received);
            assert([NSURL URLWithString:received].isFileURL);
            assert([[NSURL URLWithString:received].path isEqual:folder]);
        }
        for (NSArray *selection in @[@[], @[[NSURL fileURLWithPath:note]],
                @[[NSURL fileURLWithPath:folder], [NSURL fileURLWithPath:root]],
                @[[NSURL URLWithString:@"https://example.com/folder"]],
                @[[NSURL fileURLWithPath:folder], [NSURL URLWithString:@"https://example.com"]]]) {
            [board clearContents];
            if (selection.count) assert([board writeObjects:selection]);
            NSString *error = nil;
            received = nil;
            [provider openFolderAsVault:board userData:nil error:&error];
            if (!error || received) fprintf(stderr, "Unexpected accepted selection: %s\n", selection.description.UTF8String);
            assert(error && !received);
        }
        assert([[NSString stringWithContentsOfFile:note encoding:NSUTF8StringEncoding error:NULL] isEqual:@"original"]);
        [board releaseGlobally];
        assert([files removeItemAtPath:root error:NULL]);
        puts("Folder Service: modern + legacy folder PASS; empty/file/multiple/remote refused; source unchanged");
    }
    return 0;
}

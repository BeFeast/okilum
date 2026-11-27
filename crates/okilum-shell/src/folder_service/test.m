// Isolated AppKit pasteboard tests: no Services registration or app activation.
#import "bridge.m"
#include <assert.h>
static NSString *received;
static NSString *receivedSingleFile;
static void record(const char *url) { received = [NSString stringWithUTF8String:url]; }
static void recordSingleFile(const char *url) { receivedSingleFile = [NSString stringWithUTF8String:url]; }
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
        singleFileOpen = recordSingleFile;
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
        for (NSNumber *legacy in @[@NO, @YES]) {
            [board clearContents];
            if (legacy.boolValue) {
                [board declareTypes:@[@"NSFilenamesPboardType"] owner:nil];
                assert([board setPropertyList:@[note] forType:@"NSFilenamesPboardType"]);
            } else {
                assert([board writeObjects:@[[NSURL fileURLWithPath:note]]]);
            }
            NSString *error = nil;
            receivedSingleFile = nil;
            [provider openFileAsSingleFile:board userData:nil error:&error];
            assert(!error && receivedSingleFile);
            assert([NSURL URLWithString:receivedSingleFile].isFileURL);
            assert([[NSURL URLWithString:receivedSingleFile].path isEqual:note]);
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
        for (NSArray *selection in @[@[], @[[NSURL fileURLWithPath:folder]],
                @[[NSURL fileURLWithPath:note], [NSURL fileURLWithPath:folder]],
                @[[NSURL URLWithString:@"https://example.com/note.md"]],
                @[[NSURL fileURLWithPath:note], [NSURL URLWithString:@"https://example.com"]]]) {
            [board clearContents];
            if (selection.count) assert([board writeObjects:selection]);
            NSString *error = nil;
            receivedSingleFile = nil;
            [provider openFileAsSingleFile:board userData:nil error:&error];
            if (!error || receivedSingleFile) fprintf(stderr, "Unexpected accepted single-file selection: %s\n", selection.description.UTF8String);
            assert(error && !receivedSingleFile);
        }
        assert([[NSString stringWithContentsOfFile:note encoding:NSUTF8StringEncoding error:NULL] isEqual:@"original"]);
        [board releaseGlobally];
        assert([files removeItemAtPath:root error:NULL]);
        puts("Folder & Single-File Services: modern + legacy PASS; invalid selections refused; source unchanged");
    }
    return 0;
}

#include "testlib.h"

int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    if (ouf.readInt() != ans.readInt()) quitf(_wa, "different");
    quitf(_ok, "same");
}

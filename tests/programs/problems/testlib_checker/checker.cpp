// The verdict the solution's one token asks for: _pc(0), _pc(100) and the old quitf(_points, ...) style.
#include "testlib.h"
int main(int argc, char* argv[]) {
    registerTestlibCmd(argc, argv);
    std::string v = ouf.readToken();
    if (v == "pc0") quitf(_pc(0), "none");
    if (v == "pc100") quitf(_pc(100), "half");
    if (v == "points") quitf(_points, "%f \n partial", 0.25);
    quitf(_wa, "unknown token");
}

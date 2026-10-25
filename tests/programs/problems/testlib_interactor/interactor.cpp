// Scores in the old quitf(_points, ...) style: the points first in the message.
#include "testlib.h"
int main(int argc, char* argv[]) {
    registerInteraction(argc, argv);
    int a = inf.readInt(), b = inf.readInt();
    std::cout << a << " " << b << std::endl;
    int got = ouf.readInt();
    if (got != a + b) quitf(_wa, "expected %d, found %d", a + b, got);
    quitf(_points, "%f \nJ = %d", 0.25, got);
}

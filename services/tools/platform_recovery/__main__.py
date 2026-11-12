"""Run the application recovery contract in the released tools image."""

import sys

from platform_recovery.runtime import main

if __name__ == "__main__":
    sys.exit(main())

from setuptools import setup, find_packages

setup(
    name="auto2fa",
    version="0.2.0",
    packages=find_packages(),
    install_requires=[
        "textual>=0.40",
        "rich",
        "pyotp",
        "pexpect",
        "python-dotenv",
        "keyring",
    ],
    entry_points={
        "console_scripts": [
            "auto2fa=auto2fa.main:main",
            "auto2fa-daemon=auto2fa.daemon:main",
        ],
    },
    author="Auto2FA Team",
    description="A robust, multi-server SSH manager with 2FA automation and a TUI dashboard.",
    python_requires=">=3.6",
)

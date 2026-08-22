"""Conan recipe for the Auru PM C++ client."""

from conan import ConanFile
from conan.tools.build import check_min_cppstd
from conan.tools.cmake import CMake, CMakeDeps, CMakeToolchain, cmake_layout
from conan.tools.files import copy, load
import os
import re


class AuruPmConan(ConanFile):
    name = "auru-pm"
    package_type = "library"

    license = "MIT OR Apache-2.0"
    homepage = "https://github.com/auru-studio/auru-pm-core"
    url = "https://github.com/auru-studio/auru-pm-core"
    description = (
        "Client for auru-pm-v1 project-management providers. Connects to any endpoint "
        "you give it."
    )
    topics = ("auru", "daw", "ableton", "fl-studio", "dawproject", "version-control")

    settings = "os", "arch", "compiler", "build_type"
    options = {
        "shared": [True, False],
        "fPIC": [True, False],
        # Off by default so that depending on this package never drags libcurl
        # into a build. Most applications that would use this already have an
        # HTTP stack, and `Transport` is an interface precisely so they can plug
        # theirs in.
        "with_curl": [True, False],
    }
    default_options = {"shared": False, "fPIC": True, "with_curl": False}

    exports_sources = "CMakeLists.txt", "include/*", "src/*", "cmake/*", "README.md"

    def set_version(self):
        """Take the version from CMakeLists rather than repeating it here.

        Two declarations of one version is one too many; the second is always
        the one that goes stale.
        """
        cmake = load(self, os.path.join(self.recipe_folder, "CMakeLists.txt"))
        self.version = re.search(r"VERSION\s+(\d+\.\d+\.\d+)", cmake).group(1)

    def config_options(self):
        if self.settings.os == "Windows":
            del self.options.fPIC

    def configure(self):
        if self.options.shared:
            self.options.rm_safe("fPIC")

    def requirements(self):
        if self.options.with_curl:
            self.requires("libcurl/[>=8.0 <9]")

    def validate(self):
        check_min_cppstd(self, 17)

    def layout(self):
        cmake_layout(self)

    def generate(self):
        toolchain = CMakeToolchain(self)
        # The tests build a real auru-pm-server and talk to it over a socket;
        # that belongs in this repository's CI, not in every consumer's build.
        toolchain.cache_variables["AURU_PM_BUILD_TESTS"] = False
        toolchain.cache_variables["AURU_PM_WITH_CURL"] = bool(self.options.with_curl)
        toolchain.generate()
        CMakeDeps(self).generate()

    def build(self):
        cmake = CMake(self)
        cmake.configure()
        cmake.build()

    def package(self):
        copy(
            self,
            "README.md",
            src=self.source_folder,
            dst=os.path.join(self.package_folder, "licenses"),
        )
        CMake(self).install()

    def package_info(self):
        # `find_package(auru_pm)` and `auru::pm` work the same whether the
        # package came from Conan or from a plain CMake install, so a consumer
        # can switch between them without touching their build.
        self.cpp_info.set_property("cmake_file_name", "auru_pm")

        self.cpp_info.components["pm"].set_property("cmake_target_name", "auru::pm")
        self.cpp_info.components["pm"].libs = ["auru_pm"]
        if self.settings.os in ("Linux", "FreeBSD"):
            self.cpp_info.components["pm"].system_libs = ["m"]

        if self.options.with_curl:
            self.cpp_info.components["pm_curl"].set_property(
                "cmake_target_name", "auru::pm_curl"
            )
            self.cpp_info.components["pm_curl"].libs = ["auru_pm_curl"]
            self.cpp_info.components["pm_curl"].requires = ["pm", "libcurl::libcurl"]

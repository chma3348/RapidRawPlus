/**
 * A file or folder name as people should read it.
 *
 * Names kept on a network drive (SMB, so also cloud folders backed by one)
 * cannot end in a space or a period; macOS stores those as the private
 * characters U+F028 and U+F029, which show as boxes. Only for showing: the
 * real name, and the path, keep them.
 */
export const shownName = (name: string) => name.replace(//g, ' ').replace(//g, '.').trimEnd();

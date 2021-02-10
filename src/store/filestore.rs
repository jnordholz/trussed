use core::convert::TryInto;

use crate::{
    error::{Error, Result},
    // service::ReadDirState,
    store::{self, Store},
    types::{Message, StorageLocation as Location},
};

#[derive(Clone)]
pub struct ReadDirState {
    real_dir: PathBuf,
    last: usize,
}

use littlefs2::{fs::DirEntry, path::{Path, PathBuf}};
pub type ClientId = PathBuf;

pub struct ClientFilestore<S>
where
    S: Store,
{
    client_id: ClientId,
    store: S,
}

impl<S: Store> ClientFilestore<S> {
    pub fn new(client_id: ClientId, store: S) -> Self {
        Self { client_id, store }
    }

    /// Client files are store below `/<client_id>/dat/`.
    pub fn actual_path(&self, client_path: &PathBuf) -> PathBuf {
        let mut path = PathBuf::new();
        path.push(&self.client_id);
        path.push(b"dat\0".try_into().unwrap());
        path.push(client_path);
        path
    }

    pub fn client_path(&self, actual_path: &Path) -> PathBuf {
        let bytes = actual_path.as_ref().as_bytes();
        let absolute = bytes[0] == b'/';
        let offset = if absolute { 1 } else { 0 };

        // we know `client_id` here, could use its length
        let end_of_namespace = bytes[1..].iter().position(|&x| x == b'/')
            // oh oh oh
            .unwrap();
        let buf = PathBuf::from(&bytes[end_of_namespace + 1 + offset..]);
        // info_now!("buf out: {:?}", &buf).ok();
        buf
    }
}

pub trait Filestore {
    fn read(&mut self, path: &PathBuf, location: &Location) -> Result<Message>;
    fn write(&mut self, path: &PathBuf, location: &Location, data: &[u8]) -> Result<()>;
    fn remove_file(&mut self, path: &PathBuf, location: &Location) -> Result<()>;
    fn remove_dir(&mut self, path: &PathBuf, location: &Location) -> Result<()>;

    /// Iterate over entries of a directory (both file and directory entries).
    ///
    /// This function is modeled after `std::fs::read_dir`, within the limitations of our setup.
    ///
    /// The `not_before` parameter is an optimization for users to locate a specifically named
    /// file in one call - if the filename exists (e.g., `my-data.txt`), then return it directly.
    ///
    /// In case an entry was found, the returned option also contains state, so the expected
    /// call to `read_dir_next` can resume operation.
    fn read_dir_first(&mut self, dir: &PathBuf, location: Location, not_before: Option<&PathBuf>)
        -> Result<Option<(DirEntry, ReadDirState)>>;

    /// Continue iterating over entries of a directory.
    ///
    /// Return the entry just after the previous one. If it exists, also return state for the
    /// following call.
    fn read_dir_next(&mut self, state: ReadDirState)
        -> Result<Option<(DirEntry, ReadDirState)>>;

    // fn read_dir_files_first(&mut self, dir: &PathBuf, location: Location, not_before: Option<&PathBuf>)
    //     -> Result<Option<(DirEntry, ReadDirState)>>;
}

impl<S: Store> Filestore for ClientFilestore<S> {
    fn read(&mut self, path: &PathBuf, location: &Location) -> Result<Message> {
        let path = self.actual_path(path);

        store::read(self.store, *location, &path)
    }

    fn write(&mut self, path: &PathBuf, location: &Location, data: &[u8]) -> Result<()> {
        let path = self.actual_path(path);
        store::write(self.store, *location, &path, data)
    }

    fn remove_file(&mut self, path: &PathBuf, location: &Location) -> Result<()> {
        let path = self.actual_path(path);

        match store::delete(self.store, location.clone(), &path) {
            true => Ok(()),
            false => Err(Error::InternalError),
        }
    }

    fn remove_dir(&mut self, path: &PathBuf, location: &Location) -> Result<()> {
        let path = self.actual_path(path);

        match store::delete(self.store, location.clone(), &path) {
            true => Ok(()),
            false => Err(Error::InternalError),
        }
    }

    fn read_dir_first(&mut self, clients_dir: &PathBuf, location: Location, not_before: Option<&PathBuf>) -> Result<Option<(DirEntry, ReadDirState)>> {
        if location != Location::Internal {
            return Err(Error::RequestNotAvailable);
        }
        let fs = self.store.ifs();

        let dir = self.actual_path(clients_dir);

        //// this is only a Result instead of an Option because `read_dir_and_then` works this way
        //let look_what_i_found = fs.read_dir_and_then(&dir, |it| {

        //    // iterate over entries in the directory
        //    // skip `.` and `..`
        //    for (i, entry) in it.enumerate().skip(2) {
        //        // why is entry a Result?
        //        let entry = entry.unwrap();
        //        match not_before {
        //            None => return Ok((i, entry)),
        //            Some(not_before) => {
        //                if entry.file_name() == not_before.as_ref() {
        //                    return Ok((i, entry));
        //                }
        //            }
        //        }
        //    }

        //    //
        //    Err(littlefs2::io::Error::Io)
        //});

        //Ok(match look_what_i_found {
        //    Ok((i, mut entry)) => {
        //        let read_dir_state = ReadDirState {
        //            real_dir: dir,
        //            last: i
        //        };

        //        let entry_client_path = self.client_path(entry.path());
        //        *unsafe { entry.path_buf_mut() } = entry_client_path;
        //        Some((entry, read_dir_state))
        //    }

        //    _ => None,

        //})

        Ok(fs.read_dir_and_then(&dir, |it| {

            // this is an iterator with Item = (usize, Result<DirEntry>)
            it.enumerate()

                // skip over `.` and `..`
                .skip(2)

                // todo: try ?-ing out of this (the API matches std::fs, where read/write errors
                // can occur during operation)
                .map(|(i, entry)| (i, entry.unwrap()))

                // if there is a "not_before" entry, filter out entries before it
                .filter(|(_, entry)| {
                    match not_before {
                        Some(not_before) => entry.file_name() == not_before.as_ref(),
                        _ => true,
                    }
                })

                // take first entry that fits the bill
                .next()

                // if there is an entry, construct the state that needs storing out of it,
                // remove the prefix from the entry's path to not leak implementation details to
                // the client, and return both the entry and the state
                .map(|(i, mut entry)| {
                    let read_dir_state = ReadDirState { real_dir: dir.clone(), last: i };
                    let entry_client_path = self.client_path(entry.path());
                    // This is a hidden function which allows us to modify `entry.path`.
                    // In regular use, `DirEntry` is not supposed to be constructable by the user
                    // (only by querying the filesystem), which is why the function is both
                    // hidden and tagged "unsafe" to discourage use. Our use case here is precisely
                    // the reason for its existence :)
                    *unsafe { entry.path_buf_mut() } = entry_client_path;
                    (entry, read_dir_state)

                // the `ok_or` dummy error followed by the `ok` in the next line is because
                // `read_dir_and_then` wants to see Results (although we naturally have an Option
                // at this point)
                }).ok_or(littlefs2::io::Error::Io)
        }).ok())
    }

    fn read_dir_next(&mut self, state: ReadDirState) -> Result<Option<(DirEntry, ReadDirState)>> {

        let ReadDirState { real_dir, last } = state;
        let fs = self.store.ifs();

        // all we want to do here is skip just past the previously found entry
        // in the directory iterator, then return it (plus state to continue on next call)
        let look_what_i_found = fs.read_dir_and_then(&real_dir, |it| {

            // skip over previous
            it.enumerate().skip(last).next()
                // entry is still a Result :/ (see question in `read_dir_first`)
                .map(|(i,entry)| (i, entry.unwrap()))
                // convert Option into Result, again because `read_dir_and_then` expects this
                .ok_or(littlefs2::io::Error::Io)

        }).ok();

        Ok(look_what_i_found
            .map(|(i, mut entry)| {
                let read_dir_state = ReadDirState { real_dir, last: i };

                let entry_client_path = self.client_path(entry.path());
                *unsafe { entry.path_buf_mut() } = entry_client_path;

                (entry, read_dir_state)
            }))
    }
}
